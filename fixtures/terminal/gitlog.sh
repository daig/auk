#!/bin/sh
# Builds a small repository with fixed dates, branches, a merge and UTF-8 messages, then runs
# `git log --color` through less (git's pager) as the recording scenario drives it.
set -e
export GIT_CONFIG_NOSYSTEM=1 GIT_AUTHOR_NAME="Ada Lovelace" GIT_AUTHOR_EMAIL=ada@example.com
export GIT_COMMITTER_NAME="Ada Lovelace" GIT_COMMITTER_EMAIL=ada@example.com
git init -q -b main repo && cd repo
n=0
commit() {
  n=$((n + 1))
  export GIT_AUTHOR_DATE="2026-01-0${2:-1}T12:00:0${n#${n%?}}Z" GIT_COMMITTER_DATE="2026-01-0${2:-1}T12:00:0${n#${n%?}}Z"
  printf '%s\n' "$1" >>"file$n.txt"
  git add -A && git commit -q -m "$1"
}
commit "Initial commit: README と設定" 1
commit "Add the parser (パーサー) with a very long subject line that will certainly wrap in an eighty column terminal window" 1
git checkout -q -b feature
commit "feature: wide 字符 handling 🦀" 2
commit "feature: emoji 🎉 width tables" 2
git checkout -q main
commit "main: fix the scroll region bug" 3
git merge -q --no-ff -m "Merge branch 'feature' — 合并" feature
git tag v1.0
commit "Release notes ✨" 4
git log --color --graph --decorate --stat --all
