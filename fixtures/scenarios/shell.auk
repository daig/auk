# A shell session: commands, colored output and the prompt (auk shot, §11.3).
# Expects the test shell config (PS1='%~ %# ') and HOME as its directory.
size 720x360
ready
run "echo hello from auk"
expect "hello from auk"
run 'printf "\e[1;31mred\e[0m \e[4;32munderlined green\e[0m \e[7minverse\e[0m\n"'
expect "underlined green"
type "echo unfinished"
shot shell
