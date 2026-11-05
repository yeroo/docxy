# Project scheduling

Notes moved from `CLAUDE.md` (#1903). Read them before changing Project's scheduling: leveling and elapsed durations. This file is not loaded automatically: the `CLAUDE.md` index (Area notes) points here. Paths are from the repository root. Add this area's new notes here as `##` sections.

## Project leveling as a plan edit (#1594)

- `projcore::schedule::level_with(proj, &LevelScope {movable, order,
  within_slack})` levels a scope: tasks outside it, and priority 1000, are
  never delayed and are booked at the dates the plan gives them now (a held
  automatic task over each assignment's own window, a manual or tracked one
  its whole span; an automatic follower of a moved task still follows its
  links once the delays are written, so the pass repeats on the plan with
  its delays written while a round still changes the overallocation
  (`schedule::overallocation`: a smaller overbooked extent on the committed
  schedule, or conflicts not seen before, as when one is traded for another,
  heavier or not; every overbooked stretch's tasks make the signature, dates
  left out), drops a round that leaves the same conflicts (a follower moving
  in lockstep with its predecessor would ask for more delay for ever; one
  that would carry the pair into a later stretch of more capacity is dropped
  the same way, a known limit: select the follower too), ends
  on the lightest plan it reached (within slack a task whose own delay would
  move the project finish or turn a task's total slack negative, a written
  delay stretching a summary's span and a percentage lag on the summary's
  link with it, is held at its earliest start and the round rerun without
  it; a break no task explains alone or by its restoration drops the round),
  and reports a conflict that remains as
  overallocated: a later pass resolves it only with the
  follower in its scope or with `clear_first` off, the default rebuilding the
  same cleared plan); an ALAP task is neither delayed nor
  booked (Project 2024 refuses a delay on one, measured, so the schedule
  ignores a stored one, and its late dates float with the finish);
  `within_slack` leaves a task that would start past its CPM late start at
  its earliest start, overallocated. `Leveled::delay_min`
  is each leaf's own delay in elapsed minutes, from the start its leveled
  predecessors drive it to, so a task that only follows a delayed
  predecessor records none (Project 2024 over COM: the FS successor of a
  leveled task keeps LevelingDelay 0). The CPM forward pass adds a stored
  task `LevelingDelay` (tenths of a minute, elapsed) to the snapped driven
  start and snaps again; the backward pass bounds a delayed task's
  predecessors by its late start less the delay, the late finish following
  through the task's working span (`late_bounds_before_delay`, on direct and
  summary links alike), so they report the zero slack Project 2024 shows
  them (measured; a delayed milestone's bound is its predecessor's finish
  instant). The leveler places a task from where its links drive it after
  its predecessors' moves, each link's demand rebuilt per predecessor: a
  working-lag link's is its CPM demand shifted by that predecessor's own
  placement delay in the default calendar's index (the leveler's booking
  space), an elapsed lag's counts from the predecessor's leveled instant, a
  summary's never pulls before its CPM demand; the latest wins (demands on
  one index by instant), snapped on the task's own calendar, and a
  same-calendar FS milestone keeps its predecessor's finish instant. That
  demand goes through the task's own constraint as the forward pass applies
  it: an honored Must Start/Finish On keeps its date, an honored Start/Finish
  No Later Than takes the earlier of demand and cap (a binding cap holds a
  milestone at the constraint's own instant), every other constraint is a
  floor the demand may pass, two instants on one working index compared as
  instants (a summary predecessor's demand included); so
  a Start No Earlier Than, a cap, or another predecessor that still drives
  later absorbs the move and the task takes no delay
  (`Schedule::delay_driven` holds the start a stored delay was added to). A
  kept delay is rebuilt from that driven start plus the delay, snapped on
  the task's own calendar, and its own increment is measured from the
  unsnapped delayed instant, so the forward pass's one snap of driven + kept
  + own lands on the placement. A milestone's reported start is that snap
  when a kept delay put it there, else the demand the placement took (an FS
  evening, a held cap instant, a snapped start-type demand), as the forward
  pass starts it. A task on another calendar whose predecessor
  moved is estimated at the default calendar's next working minute (its
  committed dates are the forward pass's), so its own delay there may need a
  further pass. `Schedule::start_before_delay` is what Move Task counts
  from, an inactive task's included, so a saved delay is not added twice. A
  delayed milestone's late bound keeps its predecessor's finish instant on an
  FS link only; on a start-type link the inverse walks the forward pass's
  snap back, a fixed successor's cap on its ALAP predecessors counts the
  intervening tasks' stored delays but never a summary successor's (the
  forward pass applies no summary's), and an unlinked task's pre-start
  window reaches its early start as the forward pass bounded it. A
  summary link's free-slack bound takes each successor's delay before the
  group's minimum; the timeline's horizon budgets stored delays; equal
  priorities level in readiness order.
- `Editor::level_tasks(&LevelTarget::{All, Selection, Resource})`
  (`level_tasks_with` takes `LevelOptions` for one pass) writes
  `leveling_delay` (format 8) and `pre_leveled_start/finish` (from the plan
  without the scope's delays, whatever `clear_first` says, so a repeated
  pass records no step) as one undo step: only the delays the leveler asks
  for, so a task a constraint or another predecessor already holds takes
  none (as Project leaves it), and the reply (`delayed`, `overallocated`,
  the tasks' starts) reports the committed schedule, not the display
  overlay. `projctl::mutates(verb, args)` is the hosts' edit test: a bare
  `level.options {}` is a read. With
  `clear_first` a priority-1000 task's stored delay is cleared too and the
  task holds at its unleveled start (measured). `clear_leveling(uids | all)`
  is one step. `LevelOptions` are the plan options `LevelingOrder`,
  `LevelWithinSlack`, `LevelAdjustAssignments`, `LevelCanSplit`,
  `LevelClearBeforeLeveling`. Level All stays the display overlay in both
  hosts; the edits are `projctl`'s `level.selection {uids, options?}`,
  `level.resource {resource, options?}`, `level.options` and `level.clear
  {uids | all}`; `LevelOutcome::targeted` lists the leaves a reply reports.

## Elapsed task durations and MSO before opening (#484)

- A leaf whose `DurationFormat` is elapsed (4, 6, 8, 10, 12, estimated or
  not: `Task::elapsed_duration`) holds **elapsed (wall-clock) minutes** in
  `duration_min`, as MSPDI `<Duration>` states them; every other leaf holds
  working minutes. The editor computes or parses only working minutes, so
  every write of some over an elapsed task (a typed duration through
  `update_task_with`, a manual task's typed finish, every progress setter)
  first re-expresses the whole task in working time, in the same undo step
  (`editor/elapsed.rs`, `Editor::elapsed_to_working` run by
  `in_working_time`), then runs exactly as on a working task. Its window
  is its elapsed duration from its actual start, else its scheduled start
  (never the tracked schedule's span, which counts it as working minutes);
  the duration becomes the working minutes the window covers, the actual,
  remaining and manual durations scale alike, and each work assignment's
  work, scheduled overtime and actual (and actual overtime) work scale by
  one factor, units kept for every task type: a flat assignment of an
  untracked task its own span's (`Editor::own_spans`: working over
  wall-clock minutes of the span `assignment_span` dated, from its actual
  start or working Delay to its actual finish or work / units on the
  clock), so it keeps its dates over a weekend, delayed, shorter, or with
  its own actuals; contoured work and a tracked task's the window's. A
  span holding no working time (an evening after an FS link) keeps its
  work (a factor of 1, as a progress write keeps a working task's), so in
  working time its work runs from the next working minute and, when longer
  than the task's working minutes, finishes after its task; with actual work there the write is refused
  (`ELAPSED_ASSIGNMENT_NO_WORKING_TIME`). Only figures the factor
  changes are rewritten, so a factor of 1 keeps the priced fields; where
  the work changes the refresh derives what `set_work` clears (regular
  work, costs, remaining overtime) and the planned spread is dropped;
  dates, delays, baselines, earned value, budgets and protected actuals
  stay. The format moves to the same unit in working
  time. A window with no working time (an FS follower over
  a night or a weekend), or no start, refuses a progress write and any write
  on a task with progress; a write that replaces the duration (a typed
  duration, Ctrl+Delete, a milestone toggle, a manual finish) takes the task
  to zero working minutes first, its work as for a duration going to zero
  (`rescale_work`: units kept, Fixed Work keeps its work). A progress write
  or manual finish that fails or changes nothing rolls the conversion back
  by hand (`Editor::savepoint`/`rollback_to`), also inside an outer batch
  (projctl `task.set`, a paste); a typed duration is always a change, even
  of the minutes the conversion gave (`1d` on `1 eday` from a Monday
  morning). Any duration on an elapsed task is working minutes, whatever
  its number, so a copied elapsed cell pasted back (`9d` for `3 edays`)
  is nine working days: that edit text is a display follow-up. An auto
  task's typed finish is a constraint and keeps the format, and an effort recalculation (work / units, the work counted over
  the elapsed minutes) is elapsed minutes and keeps it too. Unmeasured:
  Project keeps progress and typed durations on an elapsed duration.
- The schedule runs an automatic elapsed leaf on the 24x7 clock, start
  included (`Scheduler::elapsed`): it starts at its driven or constraint
  instant unsnapped (unlinked: the raw project start) and finishes its
  duration later; its late finish is the latest instant each successor
  allows (`elapsed_pred_bound`: the morning side of the successor's index,
  or its exact instant for an elapsed successor or an FS milestone) and its
  late start that duration before. Its four slacks are wall-clock minutes
  (late less early instants). An assignment's Delay is working time on
  every task (a LevelingDelay by its format), so on an elapsed task too,
  and the conversion leaves it as it is; a flat-work assignment's work /
  units then runs on the clock (`schedule::on_the_clock`,
  `assign::assignment_span`), so an undelayed one at the units its work was
  counted at finishes with the task. A delayed one does not extend an
  elapsed task in the scheduler (`delayed_extent_min` is none there: a
  working delay plus wall-clock work reaches a point that depends on the
  task's start), so a plan read with one may finish it after its task (a
  known limit). The editor's effort and units math (`same_resources`'s
  longest `work / units + delay`, a Fixed Duration task's span after the
  delay, the units stages) takes the Delay in the task's unit
  (`assign::delay_in_task_unit`: on an elapsed task, the wall-clock minutes
  from its start to where the working Delay lands), so an edit there does
  store a duration that covers the delayed assignment, which then finishes
  with its task. Unmeasured; no corpus assignment is on an elapsed task. Measured (Project 2024): unlinked, on
  08:00 and 08:30 calendars (snapshots 15..46 UID 25: start 08:00, finish
  +72 h, slack 260400 tenths). Unmeasured, following the rule: links,
  constraints, ALAP, a stored leveling delay (added unsnapped), a percent
  lag off an elapsed predecessor (a share of its elapsed minutes). The
  leveler holds an elapsed task at its CPM dates, booked over its span, and
  does not place it from leveled predecessors either: an elapsed task
  FS-linked from a task Level All delays is drawn before that predecessor's
  leveled finish (a known limit). A tracked elapsed task is scheduled from
  its actuals as working minutes, as before (`Scheduler::elapsed` is
  `None`): a complete one sits on its actual dates, but an in-progress
  one's wall-clock actual and remaining durations would be misplaced, so a
  plan with one keeps every stored date on save (`schedule_reproduces`),
  and the `.mpp` import gives an in-progress auto row its working span and
  no elapsed format, as before #484. The grid and Markdown show the
  elapsed slack as working days (a display follow-up); terminal `yppxy` gets
  all of it through projcore.
- An honored Must Start On whose instant is before a working day's first
  working minute keeps that instant as its start; its index, so its finish,
  is the opening's (snapshots 31..46 UID 30: 08:00..17:30 on a calendar that
  opens at 08:30, late start 08:00). A date in a lunch break or on a
  nonworking day still snaps (unmeasured).
- `mspdi::schedule_reproduces` no longer gives up on an untracked or
  complete elapsed task, so such a plan writes scheduled dates and
  wall-clock slack. The `.mpp` import keeps an auto task's elapsed
  Duration (FixedData +84) and format (`mpp::elapsed_duration_format`)
  unless it is in progress (`mpp::carries_duration_format`, which the
  oracle shares); a manual task's elapsed duration is not decoded (#225). `oracle_corpus` compares UID 25 (dates, late dates,
  slacks, imported unpinned) and UID 30 in every snapshot that has them.
