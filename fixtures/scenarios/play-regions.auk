# surface-play of the book's inline surface with main, dock and layer regions (§7.5, §7.15).
size 800x420
ready
surface-play {fixtures}/tsp/docs-regions.jsonl
expect "Run cargo update -p serde?"
shot regions
