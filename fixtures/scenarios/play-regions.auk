# surface-play of the inline surface from Tern's docs, with main, dock and layer regions.
size 800x420
ready
surface-play {fixtures}/tsp/docs-regions.jsonl
expect "Run cargo update -p serde?"
shot regions
