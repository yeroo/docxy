<#
.SYNOPSIS
  Generate Rust #[interface] definitions for Excel's dual interfaces from the
  type-library dump, so an early-bound .NET client's vtable calls land on real
  slots (in Excel's exact order). Every slot is a stub returning E_NOTIMPL;
  real create-path methods are layered on by hand in main.rs.

.DESCRIPTION
  Reads tlb-dump.txt (produced from the live Excel typelib: one "slot#N ..." line
  per vtable entry, in oVft order). For each requested interface it emits, into
  xlcomshim/src/gen_excel.rs:
    - `#[interface("<iid>")] unsafe trait <Name>: IDispatch { unsafe fn s<slot>(&self) -> HRESULT; ... }`
      for slots 7..cFuncs-1 (0..6 are the inherited IUnknown+IDispatch slots).
    - `impl <Name>_Impl for <Struct>_Impl { unsafe fn s<slot>(&self) -> HRESULT { E_NOTIMPL } ... }`
  The generated file is `include!`d inside `mod win`, so it shares its imports.
#>
[CmdletBinding()]
param(
    [string]$Dump = "$env:LOCALAPPDATA\Temp\claude\C--Users-boris-source-docxy\72907cf6-fe7e-4b9c-9382-665f995312a1\scratchpad\tlb-dump.txt",
    [string]$Out
)
$ErrorActionPreference = 'Stop'
if (-not $Out) { $Out = Join-Path $PSScriptRoot '..\..\xlcomshim\src\gen_excel.rs' }

# dump interface name -> @{ trait = <Rust interface trait>; struct = <implementing struct> }
# Trait names are I-prefixed so they never collide with the struct names
# (Range/Font/Workbooks/Interior would otherwise clash). Note both `.Sheets` and
# `.Worksheets` return the same `Sheets` interface, implemented by our Worksheets.
$map = [ordered]@{
    '_Application' = @{ trait = 'IApplication'; struct = 'Application' }
    'Workbooks'    = @{ trait = 'IWorkbooks';   struct = 'Workbooks' }
    '_Workbook'    = @{ trait = 'IWorkbook';    struct = 'Workbook' }
    'Sheets'       = @{ trait = 'ISheets';      struct = 'Worksheets' }
    '_Worksheet'   = @{ trait = 'IWorksheet';   struct = 'Worksheet' }
    'Range'        = @{ trait = 'IRange';       struct = 'Range' }
    'Font'         = @{ trait = 'IFont';        struct = 'Font' }
    'Interior'     = @{ trait = 'IInterior';    struct = 'Interior' }
}

# Real create-path members: specific vtable slots get a real signature + a body
# that calls a hand-written handler in main.rs, instead of the E_NOTIMPL stub.
# Keyed by dump interface name, then slot number => @{ sig = <params after &self>;
# call = <expr returning HRESULT> }. Slots + param shapes come from the typelib
# dump; the LCID (`_lcid: u32`) params come from the PIA's [LCIDConversion] tags
# (tools/comshim/excel-pia.txt) -- the CLR INJECTS an lcid arg at that managed
# position which the typelib does NOT show, so it must be in the vtable sig or
# every arg after it lands in the wrong slot. [in] VARIANT is a 24-byte by-value
# struct => hidden pointer on x64, so it is `*const VARIANT`. Handlers ignore the
# lcid; it exists only to keep the ABI aligned.
$V = '*const VARIANT'
$overrides = @{
    '_Application' = @{
        52  = @{ sig = "ret: *mut *mut c_void";        call = 'vt_app_workbooks(self, ret)' }
        119 = @{ sig = "_lcid: u32, ret: *mut i16";    call = 'vt_app_da_get(self, ret)' }
        120 = @{ sig = "_lcid: u32, v: i16";           call = 'vt_app_da_put(self, v)' }
        197 = @{ sig = "ret: *mut BSTR";               call = 'vt_app_name(self, ret)' }
        230 = @{ sig = "";                             call = 'vt_app_quit(self)' }
        281 = @{ sig = "_lcid: u32, ret: *mut BSTR";   call = 'vt_app_version(self, ret)' }
        282 = @{ sig = "_lcid: u32, ret: *mut i16";    call = 'vt_app_vis_get(self, ret)' }
        283 = @{ sig = "_lcid: u32, v: i16";           call = 'vt_app_vis_put(self, v)' }
    }
    'Workbooks' = @{
        10 = @{ sig = "_tmpl: $V, _lcid: u32, ret: *mut *mut c_void"; call = 'vt_wbs_add(self, ret)' }
        12 = @{ sig = "ret: *mut i32";                    call = 'vt_wbs_count(self, ret)' }
        13 = @{ sig = "index: $V, ret: *mut *mut c_void"; call = 'vt_wbs_item(self, index, ret)' }
        17 = @{ sig = "index: $V, ret: *mut *mut c_void"; call = 'vt_wbs_item(self, index, ret)' }
    }
    '_Workbook' = @{
        27  = @{ sig = "_sc: $V, _fn: $V, _rw: $V, _lcid: u32"; call = 'vt_wb_close(self)' }
        66  = @{ sig = "ret: *mut BSTR";              call = 'vt_wb_name(self, ret)' }
        105 = @{ sig = "_lcid: u32, ret: *mut i16";   call = 'vt_wb_saved_get(self, ret)' }
        106 = @{ sig = "_lcid: u32, v: i16";          call = 'vt_wb_saved_put(self, v)' }
        112 = @{ sig = "ret: *mut *mut c_void";       call = 'vt_wb_sheets(self, ret)' }
        131 = @{ sig = "ret: *mut *mut c_void";       call = 'vt_wb_sheets(self, ret)' }
        # PIA binds managed SaveAs to _SaveAs (slot 172): 12 params + LCID@12.
        172 = @{ sig = "filename: $V, fileformat: $V, _p3: $V, _p4: $V, _p5: $V, _p6: $V, _access: i32, _p8: $V, _p9: $V, _p10: $V, _p11: $V, _p12: $V, _lcid: u32"; call = 'vt_wb_saveas(self, filename, fileformat)' }
    }
    'Sheets' = @{
        12 = @{ sig = "ret: *mut i32";                    call = 'vt_sheets_count(self, ret)' }
        15 = @{ sig = "index: $V, ret: *mut *mut c_void"; call = 'vt_sheets_item(self, index, ret)' }
        25 = @{ sig = "index: $V, ret: *mut *mut c_void"; call = 'vt_sheets_item(self, index, ret)' }
    }
    '_Worksheet' = @{
        18  = @{ sig = "ret: *mut BSTR";                            call = 'vt_ws_name_get(self, ret)' }
        19  = @{ sig = "v: *const u16";                            call = 'vt_ws_name_put(self, v)' }
        52  = @{ sig = "ret: *mut *mut c_void";                     call = 'vt_ws_cells(self, ret)' }
        100 = @{ sig = "cell1: $V, cell2: $V, ret: *mut *mut c_void"; call = 'vt_ws_range(self, cell1, cell2, ret)' }
    }
    'Range' = @{
        52  = @{ sig = "row: $V, col: $V, ret: *mut VARIANT"; call = 'vt_rng_child(self, row, col, ret)' }
        53  = @{ sig = "row: $V, col: $V, val: $V";           call = 'vt_rng_item_put(self, row, col, val)' }
        71  = @{ sig = "ret: *mut VARIANT";                   call = 'vt_rng_formula_get(self, ret)' }
        72  = @{ sig = "v: $V";                               call = 'vt_rng_formula_put(self, v)' }
        100 = @{ sig = "row: $V, col: $V, ret: *mut VARIANT"; call = 'vt_rng_child(self, row, col, ret)' }
        101 = @{ sig = "row: $V, col: $V, val: $V";           call = 'vt_rng_item_put(self, row, col, val)' }
        180 = @{ sig = "_ty: $V, ret: *mut VARIANT";          call = 'vt_rng_value_get(self, ret)' }
        181 = @{ sig = "_ty: $V, val: $V";                    call = 'vt_rng_value_put(self, val)' }
        182 = @{ sig = "ret: *mut VARIANT";                   call = 'vt_rng_value_get(self, ret)' }
        183 = @{ sig = "val: $V";                             call = 'vt_rng_value_put(self, val)' }
    }
}

$lines = Get-Content -LiteralPath $Dump
$sb = [System.Text.StringBuilder]::new()
[void]$sb.AppendLine('// @generated by tools/comshim/gen-vtables.ps1 from the Excel typelib. Do not edit.')
[void]$sb.AppendLine('// Full vtable slot lists (Excel oVft order) as E_NOTIMPL stubs; real methods')
[void]$sb.AppendLine('// are layered on in main.rs. include!d inside `mod win`.')
[void]$sb.AppendLine()

foreach ($iface in $map.Keys) {
    $trait  = $map[$iface].trait
    $struct = $map[$iface].struct
    # locate the interface header
    $start = ($lines | Select-String -SimpleMatch "INTERFACE $iface " | Select-Object -First 1).LineNumber
    if (-not $start) { throw "interface $iface not found in dump" }
    $header = $lines[$start - 1]
    if ($header -notmatch 'IID=\{([0-9A-Fa-f-]+)\}') { throw "no IID for $iface" }
    $iid = $Matches[1]

    # collect slot numbers >= 7 until the next INTERFACE/separator
    $slots = @()
    for ($i = $start; $i -lt $lines.Count; $i++) {
        $ln = $lines[$i]
        if ($ln -match '^INTERFACE ' -or $ln -match '^={5,}') { break }
        if ($ln -match '^\s*slot#(\d+)\s') {
            $n = [int]$Matches[1]
            if ($n -ge 7) { $slots += $n }
        }
    }

    $ov = $overrides[$iface]
    if (-not $ov) { $ov = @{} }

    [void]$sb.AppendLine("#[interface(`"$iid`")]")
    [void]$sb.AppendLine("unsafe trait $trait`: IDispatch {")
    foreach ($n in $slots) {
        if ($ov.ContainsKey($n)) {
            $sig = $ov[$n].sig
            $args = if ($sig) { ", $sig" } else { "" }
            [void]$sb.AppendLine("    unsafe fn s$n(&self$args) -> HRESULT;")
        } else {
            [void]$sb.AppendLine("    unsafe fn s$n(&self) -> HRESULT;")
        }
    }
    [void]$sb.AppendLine("}")
    [void]$sb.AppendLine()
    [void]$sb.AppendLine("impl ${trait}_Impl for ${struct}_Impl {")
    foreach ($n in $slots) {
        if ($ov.ContainsKey($n)) {
            $sig = $ov[$n].sig
            $call = $ov[$n].call
            $args = if ($sig) { ", $sig" } else { "" }
            [void]$sb.AppendLine("    unsafe fn s$n(&self$args) -> HRESULT { unsafe { $call } }")
        } else {
            [void]$sb.AppendLine("    unsafe fn s$n(&self) -> HRESULT { E_NOTIMPL }")
        }
    }
    [void]$sb.AppendLine("}")
    [void]$sb.AppendLine()
    Write-Host ("{0} -> {1} on {2}: {3} slots (7..{4}), {5} real" -f $iface, $trait, $struct, $slots.Count, ($slots[-1]), $ov.Count)
}

Set-Content -LiteralPath $Out -Value $sb.ToString() -Encoding UTF8
Write-Host "wrote $Out"
