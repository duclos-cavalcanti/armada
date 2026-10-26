# Armada TODO

- [ ] **`fleetc export` — snapshot a project's state.** `.fleet/` (the ledger, telemetry) is live
  SQLite and stays out of git. For a user who wants to keep or version a project's state, a
  consistent snapshot (`sqlite3 … .backup`) written somewhere they choose.
