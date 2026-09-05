This command was not run. It is irreversible:

{{explanation}}
Before it can proceed, stop and check it against the user's actual request:
- Which specific thing the user asked for requires deleting this?
- Did the user name this path, or did you infer it?
- If you inferred it, is a narrower target enough?
- If this is wrong, nothing here can be recovered.

If it is genuinely what the user asked for, re-issue the same call with a `justification` field explaining which request it serves. If you are not sure, ask the user instead: that costs one message, and being wrong costs their data.