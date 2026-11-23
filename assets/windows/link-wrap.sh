#!/bin/sh
# Linker for x86_64-pc-windows-gnu. Adds the admin manifest only when the
# output file is exactly PULSE.exe. The unit-test harness is PULSE-<hash>.exe
# and must stay asInvoker so `cargo test` does not request elevation.
set -eu

out=""
prev=""
for arg in "$@"; do
    if [ "$prev" = "-o" ]; then
        out=$arg
    fi
    prev=$arg
done

real=""
for candidate in x86_64-w64-mingw32-gcc x86_64-pc-windows-gnu-gcc; do
    if command -v "$candidate" >/dev/null 2>&1; then
        real=$candidate
        break
    fi
done
if [ -z "$real" ]; then
    echo "PULSE link-wrap: mingw gcc was not found" >&2
    exit 1
fi

base=$(basename "$out")
if [ "$base" != "PULSE.exe" ]; then
    exec "$real" "$@"
fi

windres=""
for candidate in x86_64-w64-mingw32-windres x86_64-pc-windows-gnu-windres windres; do
    if command -v "$candidate" >/dev/null 2>&1; then
        windres=$candidate
        break
    fi
done
if [ -z "$windres" ]; then
    echo "PULSE link-wrap: windres was not found, so PULSE.exe would not request elevation" >&2
    exit 1
fi

here=$(CDPATH= cd -- "$(dirname "$0")" && pwd)
obj=$(dirname "$out")/pulse-manifest.o
rc=$(dirname "$out")/pulse-manifest.rc
# windres wants the path inside the resource script. Forward slashes are safe.
manifest=$(printf '%s' "$here/pulse.exe.manifest" | sed 's#\\#/#g')
printf '1 24 "%s"\n' "$manifest" > "$rc"
"$windres" -O coff -i "$rc" -o "$obj"
exec "$real" "$@" "$obj"
