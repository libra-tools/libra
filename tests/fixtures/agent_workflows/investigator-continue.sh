#!/bin/sh
# AG-23 fake investigator — a CONTINUING stance.
#
# Deliberately avoids the token "conclud" so `classify_stance_disposition`
# returns Continuing (does NOT count toward quorum). Used to exhaust
# `max_turns` and as a turn-1 prior-context stance. stdin is EOF.
echo "Still gathering evidence; the startup path through cache.rs needs another pass."
echo "No firm root cause yet — keep investigating."
echo "STANCE: continuing"
