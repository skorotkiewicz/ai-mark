#!/usr/bin/env bash
set -euo pipefail
aimark_bin="$1"
demo_mode="${2:-cli}"
demo_dir=$(mktemp -d)
trap 'rm -rf -- "$demo_dir"' EXIT
mkdir -p "$demo_dir/src"
cat > "$demo_dir/src/main.rs" <<'RS'
fn main() {
    println!("written by a human");
}
RS
printf '# Example project\n\nA small provenance demo.\n' > "$demo_dir/README.md"
am() { "$aimark_bin" --root "$demo_dir" "$@"; }
am checkpoint
cat > "$demo_dir/src/main.rs" <<'RS'
fn main() {
    println!("written by a human");
    println!("written by an LLM");
}
RS
am record --harness demo --model example-model --files src/main.rs
if [[ "$demo_mode" == dashboard ]]; then
    printf 'Unrecorded notes appear as pending human lines.\n' > "$demo_dir/notes.txt"
    am dashboard
else
    am blame src/main.rs
    am stat
fi
