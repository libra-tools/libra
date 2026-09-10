#!/bin/sh
set -eu

mode=$1
log_file=$2
base_file=$3
ours_file=$4
theirs_file=$5
marker_length=$6
path_label=$7
ancestor_label=$8
ours_label=$9
theirs_label=${10}

printf '%s\n' \
    "mode=$mode" \
    "base=$(cat "$base_file")" \
    "ours=$(cat "$ours_file")" \
    "theirs=$(cat "$theirs_file")" \
    "marker=$marker_length" \
    "path=$path_label" \
    "ancestor=$ancestor_label" \
    "ours-label=$ours_label" \
    "theirs-label=$theirs_label" >>"$log_file"

case "$mode" in
    clean)
        printf 'external result\n' >"$ours_file"
        exit 0
        ;;
    empty)
        : >"$ours_file"
        exit 0
        ;;
    conflict)
        printf 'external conflict\n' >"$ours_file"
        exit 1
        ;;
    conflict128)
        printf 'external conflict 128\n' >"$ours_file"
        exit 128
        ;;
    error129)
        printf 'discarded error result\n' >"$ours_file"
        exit 129
        ;;
    signal)
        dirname "$ours_file" >"$log_file.temp-root"
        kill -TERM "$$"
        ;;
    *)
        exit 129
        ;;
esac
