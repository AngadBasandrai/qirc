#!/usr/bin/env bash
set -u
qirc="${1:-./target/release/qirc}"
files="$(ls tests/corpus/*.ll | grep -v syntax_stress) examples/redundant.ll examples/repeat_until_success.ll examples/small.ll examples/ghz.ll"
checked=0
failed=0
while IFS= read -r options; do
  for file in $files; do
    case "$file $options" in *ghz.ll*--coupling* | *ghz.ll*--calibration*) continue ;; esac
    checked=$((checked + 1))
    if ! output=$("$qirc" diff "$file" $options 2>&1); then
      failed=$((failed + 1))
      echo "FAIL $file $options"
      echo "$output" | tail -3
    fi
  done
done <<'OPTIONS'
-O3 --resynth 4
-O3 --gates rz-sx-cx --resynth 6
-O2 --cost cx
-O3 --gates rz-sx-cx --cost ibm
-O3 --gates rz-ry-cz --cost cx --resynth 3
-O3 --gates h,s,t,cx --resynth 3
-O3 --gates rx,ry,cy --cost cx
-O3 --gates rz-sx-cx --cost cx --coupling line:10
-O3 --gates rz-sx-cx --resynth 4 --coupling grid:3x4
-O3 --gates rz-sx-cx --relabel
-O2 --relabel --cost cx
-O3 --gates rz-sx-cx --calibration examples/line5.cal
-O2 --resynth 3 --cost cx
-O2 --reuse
-O3 --gates rz-sx-cx --reuse --coupling line:6
-O2 --gates rz-ry-cz --cost cx --coupling ring:12
OPTIONS
echo "sweep: $checked checked, $failed failed"
[ "$failed" -eq 0 ]
