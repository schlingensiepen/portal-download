# Contract tests

Scenarios every backend of jobs passes: the service (`cargo test` runs them through its
API) and any other implementation of the same `job.*` methods, such as a local backend of
the browser extension. Each file is one scenario.

```json
{ "name": "…", "steps": [ { "on": "a", "call": "job.create", "params": {…}, "expect": {…}, "save": {"id": "/job_id"} } ] }
```

- `on` names the backend instance, `a` when left out. Every instance starts empty, with a
  caller who may do everything on jobs.
- `call` and `params` are a JSON-RPC call. A string `"$name"` anywhere in `params` or
  `expect` is replaced by the value saved under that name; `"$today"` is today's date and
  `"$today+28"` (or `-3`) a date that many days later, both as `YYYY-MM-DD`.
- `password` is sent as the header `X-Backup-Password` with the call.
- `expect` is matched against the result: every key of an object must be there with a
  matching value, and an array must have exactly as many entries, each matching. `"$any"`
  matches every value, `"$uuid"` a GUID in the form of a version 4 UUID.
- `error` instead of `expect` names the `error.data.type` the call must fail with.
- `save` stores values of the result under a name, each given as a JSON pointer.

Times are compared in whole seconds. A run sets the due date at its own time, so a run
counts as not older than a date changed in the same second; the scenarios rely on that and
otherwise never on how fast their steps follow each other.
