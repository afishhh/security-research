#!/bin/sh
jq '[to_entries | .[] | . + {value: .value | split(., "\n")[0]} | select(.value | IN("complete", "") == false)] | from_entries' "$1"
