#!/usr/bin/env bash
set -euo pipefail

export PATH="/tmp/cds-target/debug:$PATH"

workspace="$(mktemp -d)"
home_dir="$workspace/home"
tree_root="$workspace/tree"
mkdir -p "$home_dir" "$tree_root"

seed="${CDS_RANDOM_SEED:-$RANDOM-$RANDOM-$(date +%s%N)}"
echo "cds docker cd equivalence seed: $seed"

declare -a dirs
dirs=("$tree_root")

random_label() {
    printf 'dir_%s_%04d_%04d' "$1" "$RANDOM" "$RANDOM"
}

for i in $(seq 1 36); do
    parent_index=$((RANDOM % ${#dirs[@]}))
    parent="${dirs[$parent_index]}"
    child="$parent/$(random_label "$i")"
    mkdir -p "$child"
    dirs+=("$child")

    if (( RANDOM % 3 == 0 )); then
        nested="$child/$(random_label "${i}_nested")"
        mkdir -p "$nested"
        dirs+=("$nested")
    fi
done

space_dir="$tree_root/dir with spaces"
quote_dir="$tree_root/team's app"
leading_dash_parent="$tree_root/leading"
leading_dash_dir="$leading_dash_parent/-starts-with-dash"
cdpath_root="$workspace/cdpath"
cdpath_target="$cdpath_root/cdpath_target"
logical_real="$tree_root/real/path"
logical_link="$tree_root/logical-link"

mkdir -p \
    "$space_dir" \
    "$quote_dir" \
    "$leading_dash_dir" \
    "$cdpath_target" \
    "$logical_real/child"
ln -s "$logical_real" "$logical_link"

dirs+=(
    "$space_dir"
    "$quote_dir"
    "$leading_dash_parent"
    "$leading_dash_dir"
    "$cdpath_target"
    "$logical_real"
    "$logical_real/child"
    "$logical_link"
    "$logical_link/child"
)

for dir in "${dirs[@]}"; do
    printf 'fixture file for %s\n' "$dir" > "$dir/file_$RANDOM.txt"
done

cds_init="$(cds --shell-init bash)"

setup_cds() {
    eval "$cds_init"
}

normalize_stderr() {
    local file="$1"
    local normalized
    normalized="$(mktemp)"

    sed -E \
        's#^(tests/docker_cd_equivalence\.sh: line )[0-9]+(: cd:)#\1<line>\2#' \
        "$file" > "$normalized"
    mv "$normalized" "$file"
}

run_case() {
    local mode="$1"
    local start="$2"
    local oldpwd="$3"
    local cdpath="$4"
    local label="$5"
    shift 5

    local out_file err_file state_file
    out_file="$(mktemp)"
    err_file="$(mktemp)"
    state_file="$(mktemp)"

    (
        set +e
        export HOME="$home_dir"
        export CDPATH="$cdpath"
        cd "$start" || exit 98
        export OLDPWD="$oldpwd"

        if [ "$mode" = "cds" ]; then
            setup_cds
            cds "$@"
        else
            builtin cd "$@"
        fi

        status=$?
        physical_pwd="$(pwd -P 2>/dev/null)"
        physical_status=$?
        if [ "$physical_status" -ne 0 ]; then
            physical_pwd="<pwd -P failed:$physical_status>"
        fi

        {
            printf 'label=%s\n' "$label"
            printf 'status=%s\n' "$status"
            printf 'PWD=%s\n' "$PWD"
            printf 'OLDPWD=%s\n' "${OLDPWD-}"
            printf 'PHYSICAL_PWD=%s\n' "$physical_pwd"
        } > "$state_file"
    ) > "$out_file" 2> "$err_file"

    normalize_stderr "$err_file"

    printf '%s\n%s\n%s\n' "$state_file" "$out_file" "$err_file"
}

compare_case() {
    local label="$1"
    local start="$2"
    local oldpwd="$3"
    local cdpath="$4"
    shift 4

    local cd_capture cds_capture
    cd_capture="$(run_case cd "$start" "$oldpwd" "$cdpath" "$label" "$@")"
    cds_capture="$(run_case cds "$start" "$oldpwd" "$cdpath" "$label" "$@")"

    local cd_state cd_out cd_err cds_state cds_out cds_err
    cd_state="$(printf '%s\n' "$cd_capture" | sed -n '1p')"
    cd_out="$(printf '%s\n' "$cd_capture" | sed -n '2p')"
    cd_err="$(printf '%s\n' "$cd_capture" | sed -n '3p')"
    cds_state="$(printf '%s\n' "$cds_capture" | sed -n '1p')"
    cds_out="$(printf '%s\n' "$cds_capture" | sed -n '2p')"
    cds_err="$(printf '%s\n' "$cds_capture" | sed -n '3p')"

    if ! cmp -s "$cd_state" "$cds_state" || \
       ! cmp -s "$cd_out" "$cds_out" || \
       ! cmp -s "$cd_err" "$cds_err"; then
        echo "cd equivalence failed: $label" >&2
        echo "args: $(printf '<%s> ' "$@")" >&2
        echo "--- cd state" >&2
        cat "$cd_state" >&2
        echo "--- cds state" >&2
        cat "$cds_state" >&2
        echo "--- cd stdout" >&2
        cat "$cd_out" >&2
        echo "--- cds stdout" >&2
        cat "$cds_out" >&2
        echo "--- cd stderr" >&2
        cat "$cd_err" >&2
        echo "--- cds stderr" >&2
        cat "$cds_err" >&2
        exit 1
    fi
}

compare_case "no args goes home" "$tree_root" "$space_dir" "" 
compare_case "absolute random directory" "$tree_root" "$space_dir" "" "${dirs[$((RANDOM % ${#dirs[@]}))]}"
compare_case "relative child" "$tree_root" "$space_dir" "" "$(basename "$space_dir")"
compare_case "relative parent" "$space_dir" "$tree_root" "" ..
compare_case "directory with quote" "$tree_root" "$space_dir" "" "$quote_dir"
compare_case "directory with spaces" "$tree_root" "$quote_dir" "" "$space_dir"
compare_case "leading dash via double dash" "$leading_dash_parent" "$tree_root" "" -- "-starts-with-dash"
compare_case "logical symlink default" "$tree_root" "$space_dir" "" "$logical_link/child"
compare_case "logical parent with -L" "$logical_link/child" "$tree_root" "" -L ..
compare_case "physical parent with -P" "$logical_link/child" "$tree_root" "" -P ..
compare_case "cdpath lookup" "$tree_root" "$space_dir" "$cdpath_root" cdpath_target
compare_case "cd dash" "$tree_root" "$space_dir" "" -
compare_case "invalid directory" "$tree_root" "$space_dir" "" "$tree_root/does-not-exist"
compare_case "too many args" "$tree_root" "$space_dir" "" "$space_dir" "$quote_dir"

random_cases="${CDS_DOCKER_RANDOM_CASES:-60}"
for i in $(seq 1 "$random_cases"); do
    start="${dirs[$((RANDOM % ${#dirs[@]}))]}"
    oldpwd="${dirs[$((RANDOM % ${#dirs[@]}))]}"
    target="${dirs[$((RANDOM % ${#dirs[@]}))]}"

    case $((RANDOM % 7)) in
        0) compare_case "random absolute $i" "$start" "$oldpwd" "" "$target" ;;
        1) compare_case "random dash $i" "$start" "$oldpwd" "" - ;;
        2) compare_case "random no args $i" "$start" "$oldpwd" "" ;;
        3) compare_case "random logical flag $i" "$logical_link/child" "$oldpwd" "" -L .. ;;
        4) compare_case "random physical flag $i" "$logical_link/child" "$oldpwd" "" -P .. ;;
        5) compare_case "random invalid $i" "$start" "$oldpwd" "" "$tree_root/missing_$RANDOM" ;;
        6) compare_case "random relative parent $i" "$start" "$oldpwd" "" .. ;;
    esac
done

echo "cds matched builtin cd for randomized Docker fixture"
