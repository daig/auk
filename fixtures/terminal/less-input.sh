#!/bin/sh
# Colored input for the `less` recording: SGR colors and attributes, CJK, emoji, long lines.
i=1
while [ $i -le 120 ]; do
  case $((i % 6)) in
    0) printf '\033[1;3%dm%03d bold color\033[0m 中文字符 line with a tail that is long enough to wrap past eighty columns in a terminal\n' $((i % 8)) $i ;;
    1) printf '\033[4m%03d underline\033[24m and \033[7mreverse\033[27m 🦀 crab\n' $i ;;
    2) printf '\033[38;5;%dm%03d 256-color\033[0m \033[48;2;20;40;%dmtruecolor bg\033[0m\n' $((i * 2)) $i $((i * 2)) ;;
    3) printf '%03d plain\ttabbed\tcolumns\there\n' $i ;;
    4) printf '\033[3;9m%03d italic strike\033[0m ＦＵＬＬ　ＷＩＤＴＨ\n' $i ;;
    5) printf '%03d 中文中文中文中文中文中文中文中文中文中文中文中文中文中文中文中文中文中文中文中文中文中文\n' $i ;;
  esac
  i=$((i + 1))
done
