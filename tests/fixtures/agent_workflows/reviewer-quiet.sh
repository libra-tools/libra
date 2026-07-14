#!/bin/sh
# AG-7 fake reviewer — a quiet reviewer with small, exact output.
#
# Emits exactly two short findings. The sink test asserts this verbatim block
# survives intact even while a sibling reviewer floods ~1 MiB, proving the
# per-reviewer sinks do not starve or corrupt one another. Exits 0. stdin EOF.
printf 'quiet-finding-alpha\nquiet-finding-beta\n'
