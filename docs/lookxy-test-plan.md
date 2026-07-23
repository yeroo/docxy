# lookxy — Manual Test Plan

CI exercises every code path against an in-process fake Graph server, so this
plan covers the one thing CI cannot: a real interactive session against a live
Exchange Online mailbox. Work top-to-bottom — later sections assume you are
signed in and synced.

## 0. Setup & preconditions

- [ ] **Build & run.** From a shell with the **BuildTools** `vcvars` loaded
      (the Community VC tools are broken on this box), run `cargo run -p lookxy`.
      Expect a three-pane TUI or the sign-in screen.
- [ ] **Test mailbox has the fixtures below.** Before testing, make sure the
      account contains: a plain-text mail, an HTML mail, a mail with a **file
      attachment**, a mail with an **inline image**, a mail with a **non-file
      attachment** (a forwarded `.eml`/`.ics` item), an **unread** mail, a
      **meeting invitation**, and at least a few **calendar events** today/this
      week (including one recurring series).

### Sign-in (auth-code + PKCE)

- [ ] From the sign-in screen, press **Enter** → the default browser opens the
      Microsoft login page.
- [ ] Complete login (EPAM tenant: device-code is blocked by Conditional
      Access — the **browser/loopback** flow is the supported path).
- [ ] Back in the TUI: sign-in modal closes, folders load, the message list
      populates. **Expected:** no secrets printed to the terminal at any point.
- [ ] Quit (`q`) and relaunch → it comes back **already signed in** (DPAPI token
      cache), no browser prompt.

## 1. Mail — navigation & reading

- [ ] `j`/`k` (or ↓/↑) move the message-list selection; `Enter` opens the
      message in the reading pane.
- [ ] With the reading pane focused, `j`/`k`, PageUp/PageDown, Home/End scroll
      the body.
- [ ] Open an **HTML** mail → body renders as styled text (not raw tags).
- [ ] `t` toggles **threaded ⇄ flat**; the choice persists across a relaunch.
- [ ] `/` opens search; type a term → list filters (FTS5). Confirm `q` in the
      query does **not** quit the app (it types into the query).

## 2. Mail — triage

Each action is optimistic-local (updates instantly) then drains to Graph via
the outbox. After each, confirm the change **survives a relaunch** (i.e. it
reached the server), not just the in-session view.

- [ ] `m` / `u` — mark selected read / unread (read state flips in the list).
- [ ] `f` — toggle flag (flag marker appears/clears).
- [ ] `d` or `Delete` — delete selected (moves to Deleted Items).
- [ ] `v` — move-folder popup: `j`/`k` pick a target, `Enter` moves, `Esc`
      cancels.

## 3. Mail — categories (color labels)

- [ ] `l` — category **assign** picker: `Space` toggles categories, `Enter`
      applies, `Esc` cancels. Assigned categories show as **colored dots** in
      the list and **chips** in the reader.
- [ ] `L` — category **filter**: pick one, `Enter` → list shows only mail in
      that category; clear the filter to restore.

## 4. Mail — attachments

- [ ] On a mail with a **file** attachment: `a` opens the attachments popup →
      `Enter` saves to Downloads; `o` saves **and opens** it. `Esc` closes
      without saving.
- [ ] On a mail with an **inline image**: the image is painted in the reading
      pane (falls back to a labelled box if the terminal can't do graphics).
- [ ] On a mail with a **non-file (item)** attachment: saving writes it as
      `.eml`/`.ics` (by content sniff); a **reference** attachment opens its
      link instead of saving.

## 5. Mail — compose

- [ ] `c` — new message; fill To/Subject/Body (Tab between fields); **Ctrl-Enter**
      sends. Confirm it arrives (check Sent Items / recipient).
- [ ] `r` reply, `R` reply-all, `F` forward — each opens compose pre-filled with
      the right recipients/subject/quoted body.

## 6. Mail — meeting invitations (RSVP)

Open a **meeting invitation** in the reader — an RSVP banner appears.

- [ ] `A` — **Accept** sends immediately.
- [ ] `D` — **Decline** opens the RSVP prompt: optional **proposed new
      start/end** + comment; `Tab` moves fields, `Enter` sends, `Esc` sends with
      no comment. Verify the organizer receives the response (and the proposed
      time, if set).
- [ ] `T` — **Tentative** — same prompt as Decline.

## 7. Calendar

Press `g` to toggle **Mail ⇄ Calendar** (`g` or `Esc` toggles back).

- [ ] Agenda lists upcoming events; `j`/`k` navigate. A recurring series shows a
      **"repeats"** marker.
- [ ] **Create** — `c` opens the event form: Title, Start, End, `Space` toggles
      **all-day**, Attendees, and **recurrence** fields; `Tab` between fields;
      **Ctrl-Enter** saves, `Esc` cancels. New event appears in the agenda and
      on the real calendar.
- [ ] **Recurrence** — create a **weekly** event and pick specific weekdays;
      confirm the series shows up correctly in Outlook.
- [ ] **Edit** — `e` on an event opens the form pre-filled; change something and
      Ctrl-Enter; the change persists.
- [ ] **Delete** — `x` removes the selected event.
- [ ] **RSVP to an event** — `a`/`d`/`t` on an invited event open the same
      RSVP/propose-new-time prompt as the mail reader.

### 7a. Free/busy lookup

- [ ] In the event form with one or more **attendees** entered, press
      **Ctrl-B** → the availability overlay opens (title shows the day and the
      08:00–18:00 window), one row per attendee plus a combined **`free?`** row
      (`✓` all free, `█` someone busy, `░` tentative). `Esc` returns to the form.
- [ ] With **no** attendees, the overlay shows `(no attendees)`.
- [ ] While the overlay is open, `q` does **not** quit the app (only `Esc`
      closes it).

### 7b. Reminders / alerts

- [ ] Create an event with a reminder a couple of minutes out. When it comes
      due, an **in-TUI banner** appears; `Esc` dismisses it (and does not steal
      Esc from an open search/RSVP field).
- [ ] **agwinterm notify is off by default.** Enable `reminders_notify` in the
      config, relaunch, and confirm a due reminder also raises an **agwinterm
      cross-session notification**; with it off, no agwinterm notification fires.

## 8. Automatic replies (Out-of-Office)

- [ ] `O` (works in **both** Mail and Calendar modes) opens the OOF form:
      `Tab` between fields, `Space` toggles, **Ctrl-S** saves, `Esc` cancels.
      Set an internal and external message (and a scheduled window if offered).
- [ ] Confirm in Outlook/OWA that Automatic Replies now reflects what you set;
      reopen `O` and confirm it reads back the current state.

## 9. Cross-cutting regression checks

- [ ] **`q` never quits while a modal/field is capturing keys** — verify in:
      search query, compose fields, event-form fields, the RSVP comment, the
      OOF form, and over the free/busy overlay. In each, `q` types a character
      (or is inert); it only quits from the plain list/agenda.
- [ ] **Esc precedence** — Esc closes the topmost overlay first (sign-in never
      cancels; then OOF, RSVP prompt, free/busy, category picker, attachments,
      compose, event form) and only dismisses a reminder banner when no text
      field is capturing.
- [ ] **Offline resilience** — kill the network mid-session, do a triage action
      (it applies locally), restore the network → the outbox drains and the
      change reaches the server.
- [ ] **Re-auth** — let a session run long enough for the token to refresh (or
      revoke/expire it) → sync recovers via the auth-refresh path without a full
      re-sign-in, or prompts sign-in cleanly if the refresh token is gone.

---

**Known v1 caveats (not bugs to file):**
- Windows-first: the non-Windows `tokencache` branch is `cfg`-gated and
  compile-unverified on the Windows dev box.
- Free/busy and reminders use a fixed 08:00–18:00 / per-tick model; timezone
  edge cases at extreme offsets may shift the day label by one.
