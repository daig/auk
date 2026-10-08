# The SDK's Python `table` example drawing its native surface in a shell pane (§21).
# Expects in HOME: `sdk` (the tern-sdk checkout), `python3` (a Python 3.10+), and a directory `w`
# holding the files the table lists.
size 720x360
ready
run "cd w"
run "PYTHONPATH=../sdk/python/src ../python3 ../sdk/python/examples/table.py"
expect "notes.md"
shot table
