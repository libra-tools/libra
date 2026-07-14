#!/bin/sh
# AG-23 fake investigator — a silent-but-successful turn.
#
# Emits nothing and exits 0. An empty successful turn is a STALL (not a
# stance): the run pauses as `stalled` with a `pending_turn`, resumable via
# `investigate continue`.
exit 0
