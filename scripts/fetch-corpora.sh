#!/usr/bin/env bash
# Download the standard compression benchmark corpora into ./corpora.
#
#   scripts/fetch-corpora.sh            # canterbury + silesia (~70 MB)
#   scripts/fetch-corpora.sh --all      # also large canterbury + enwik8 (~200 MB)
#
# Needs curl, tar and unzip. On Windows, run it from Git Bash.
set -euo pipefail

cd "$(dirname "$0")/.."
mkdir -p corpora
cd corpora

fetch() { # name url archive-type
    local name=$1 url=$2 kind=$3
    if [[ -d $name ]]; then
        echo "✓ $name (already present)"
        return
    fi
    echo "↓ $name"
    local tmp="$name.download"
    curl -fL --retry 3 -o "$tmp" "$url"
    mkdir "$name"
    case $kind in
        tgz) tar -xzf "$tmp" -C "$name" ;;
        zip) unzip -q "$tmp" -d "$name" ;;
    esac
    rm "$tmp"
}

# Small classic files: text, C source, HTML, a fax image, a spreadsheet, ...
fetch canterbury https://corpus.canterbury.ac.nz/resources/cantrbry.tar.gz tgz
# 12 files, 211 MB total, covering typical modern data types.
fetch silesia https://sun.aei.polsl.pl/~sdeor/corpus/silesia.zip zip

if [[ ${1:-} == --all ]]; then
    fetch large https://corpus.canterbury.ac.nz/resources/large.tar.gz tgz
    # First 100 MB of English Wikipedia: the Hutter Prize benchmark.
    fetch enwik8 http://mattmahoney.net/dc/enwik8.zip zip
fi

echo "Done. Try: cargo run --release -p cmpr-cli -- bench corpora/canterbury"
