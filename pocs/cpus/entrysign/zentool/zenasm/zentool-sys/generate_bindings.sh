#!/usr/bin/env bash

set -euo pipefail

cd "$(dirname "$0")"

if ! hash bindgen 2>/dev/null; then
	exec nix shell nixpkgs#rust-bindgen -c "$0"
fi

bindgen \
	--raw-line '#![allow(non_upper_case_globals)]' \
	--raw-line '#![allow(non_camel_case_types)]' \
	--raw-line '#![allow(non_snake_case)]' \
	--raw-line '#![allow(unsafe_op_in_unsafe_fn)]' \
	--raw-line '#![allow(improper_ctypes)]' \
	--no-prepend-enum-name \
	./header.h -- -I ../.. >src/lib.rs
