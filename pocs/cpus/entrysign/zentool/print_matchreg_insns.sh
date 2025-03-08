jq -r '.[] | split(., "\n")[0] | select(IN("complete", "", "loaded", "queried", "failed", "executed", "updated") == false)' matchreg.json
