#!/bin/zsh
set -euo pipefail

identity_change=0
expected_previous_authority=""
expected_candidate_authority=""
if [[ "${1:-}" == "--identity-change" ]]; then
    if (( $# != 5 )); then
        print -u2 "usage: check_requirements.sh --identity-change <previous.app> <candidate.app> <previous-authority> <candidate-authority>"
        exit 2
    fi
    identity_change=1
    previous="${2:A}"
    candidate="${3:A}"
    expected_previous_authority="$4"
    expected_candidate_authority="$5"
elif (( $# == 2 )); then
    previous="${1:A}"
    candidate="${2:A}"
else
    print -u2 "usage: check_requirements.sh <previous.app> <candidate.app>"
    exit 2
fi

leaf_authority() {
    /usr/bin/codesign --display --verbose=4 "$1" 2>&1 \
        | /usr/bin/sed -n 's/^Authority=//p' \
        | /usr/bin/sed -n '1p'
}

for app_path in "$previous" "$candidate"; do
    if [[ "$app_path" != */OpenCodeServer.app || ! -d "$app_path" ]]; then
        print -u2 "unexpected app bundle: $app_path"
        exit 2
    fi
    /usr/bin/codesign --verify --deep --strict "$app_path"
done

previous_requirement="$(/usr/bin/codesign -d -r- "$previous" 2>&1 | /usr/bin/sed -n 's/^designated => //p')"
candidate_requirement="$(/usr/bin/codesign -d -r- "$candidate" 2>&1 | /usr/bin/sed -n 's/^designated => //p')"
if [[ -z "$previous_requirement" || -z "$candidate_requirement" ]]; then
    print -u2 "unable to extract designated requirements"
    exit 1
fi

if (( identity_change == 1 )); then
    previous_authority="$(leaf_authority "$previous")"
    candidate_authority="$(leaf_authority "$candidate")"
    if [[ -z "$previous_authority" || -z "$candidate_authority" ]]; then
        print -u2 "unable to extract leaf signing authorities"
        exit 1
    fi
    if [[ "$previous_authority" != "$expected_previous_authority" ]]; then
        print -u2 "previous bundle signing authority does not match the declared identity-change source"
        exit 1
    fi
    if [[ "$candidate_authority" != "$expected_candidate_authority" ]]; then
        print -u2 "candidate bundle signing authority does not match the declared identity-change destination"
        exit 1
    fi
    /usr/bin/codesign --verify --strict -R="$candidate_requirement" "$candidate"
    print "identity-change previous leaf authority: $previous_authority"
    print "identity-change candidate leaf authority: $candidate_authority"
    print "identity-change previous designated requirement: $previous_requirement"
    print "identity-change candidate designated requirement: $candidate_requirement"
    print "Designated Requirements are recorded as a one-way identity transition"
    exit 0
fi

/usr/bin/codesign --verify --strict -R="$previous_requirement" "$candidate"
/usr/bin/codesign --verify --strict -R="$candidate_requirement" "$previous"
print "Designated Requirements are mutually compatible"
