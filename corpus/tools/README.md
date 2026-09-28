# Microsoft Project corpus tools

The `gen_mpp_*_cases.py` scripts use a licensed Microsoft Project desktop
installation through pywin32. They write paired native `.mpp` and Project
MSPDI `.xml` files under `corpus/mpp/`; those generated files are ignored by
Git. Run a script from the repository root, for example:

```powershell
python corpus/tools/gen_mpp_task_field_cases.py
```

The task-field generator accepts individual case function names (`flags`,
`values`, `blanks`, `overflow`, `subprojects`, `external`) as arguments. It
checks the exported XML for the requested field values before accepting a
case. Close Project gracefully if a run is interrupted.
