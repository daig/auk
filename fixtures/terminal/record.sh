#!/bin/sh
# Re-records every terminal fixture: `fixtures/terminal/record.sh [name...]`.
# Each recording's .meta.json keeps the exact command, size, environment and program version.
# Programs that show the time (htop, tmux) give different bytes on each run; the committed
# recordings are the reference inputs.
set -e
cd "$(dirname "$0")/../.."
R="python3 -I tools/vt_record.py"
F=fixtures/terminal
names="${*:-vim less htop gitlog tmux wide emoji resize}"
for name in $names; do
  case $name in
    vim) $R --out $F/vim --cols 80 --rows 24 --scenario $F/vim.scenario.json --cwd {fixtures} \
           --note "vim with the test vimrc editing sample.rs; splits and resizes" \
           -- vim -u vimrc -i NONE sample.rs ;;
    less) $R --out $F/less --cols 80 --rows 24 --scenario $F/less.scenario.json --env LESSHISTFILE=- \
           --note "less -R over colored, wide and tabbed text; search, chop toggle, resizes" \
           -- sh -c '{fixtures}/less-input.sh >{tmp}/input.txt && exec less -R {tmp}/input.txt' ;;
    htop) $R --out $F/htop --cols 100 --rows 30 --scenario $F/htop.scenario.json \
           --note "htop with no config, showing only itself and three sleeps it starts: tree view, setup screen, resizes" \
           -- sh -c 'sleep 600 & a=$!; sleep 601 & b=$!; sleep 602 & c=$!; exec htop -p $$,$a,$b,$c' ;;
    gitlog) $R --out $F/gitlog --cols 80 --rows 24 --scenario $F/gitlog.scenario.json \
           --env LESSHISTFILE=- --env LESS=FRX --env PAGER=less \
           --note "git log --color --graph --decorate --stat --all over a fixed repository, paged by less (LESS=FRX)" \
           -- sh {fixtures}/gitlog.sh ;;
    tmux) $R --out $F/tmux --cols 100 --rows 30 --scenario $F/tmux.scenario.json \
           --env TMUX_TMPDIR={tmp} --env SHELL=/bin/zsh --env PS1='%# ' \
           --note "tmux with no config on its own socket, zsh -f in each pane: two splits, zoom, resizes" \
           -- tmux -L vtrec -f /dev/null new-session 'zsh -f -i' \; set -g default-command 'zsh -f -i' \; set -g status-right 'auk %H:%M' ;;
    wide) $R --out $F/wide --cols 80 --rows 24 --scenario $F/wide.scenario.json --cwd {fixtures} \
           --env PS1='%# ' --note "zsh -f printing wide.txt (CJK, fullwidth, combining marks), resizes" \
           -- zsh -f -i ;;
    emoji) $R --out $F/emoji --cols 80 --rows 24 --scenario $F/emoji.scenario.json --cwd {fixtures} \
           --env PS1='%# ' --note "zsh -f printing emoji.txt (emoji, skin tones, ZWJ, flags, VS16), resizes" \
           -- zsh -f -i ;;
    resize) $R --out $F/resize --cols 80 --rows 24 --scenario $F/resize.scenario.json \
           --env PS1='%# ' --note "zsh -f with colored wrapped output, resized through many sizes" \
           -- zsh -f -i ;;
    *) echo "unknown recording $name" >&2; exit 1 ;;
  esac
done
