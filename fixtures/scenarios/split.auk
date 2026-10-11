# A split layout: three shells, focus moving between them.
size 800x400
ready
run "echo left"
split right
run "echo top right"
split down
run 'printf "\e[1;32mbottom right\e[0m\n"'
shot split
focus left
type "echo focused"
shot split-focus-left
