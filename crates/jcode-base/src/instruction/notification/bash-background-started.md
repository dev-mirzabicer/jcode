To wait for completion/checkpoints: use the `bg` tool with action="wait" and task_id="{{task_id}}"
To check progress immediately: use the `bg` tool with action="status" and task_id="{{task_id}}"
To see output: use the `read` tool on the output file, or `bg` with action="output"