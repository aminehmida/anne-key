#!/bin/sh

set -eux

main() {
        # This fetches latest stable release of Xargo
        local tag=$(git ls-remote --tags --refs --exit-code https://github.com/japaric/xargo \
                        | cut -d/ -f3 \
                        | grep -E '^v[0.3.0-9.]+$' \
                        | sort --version-sort \
                        | tail -n1)

        curl -LSfs https://japaric.github.io/trust/install.sh | \
            sh -s -- \
               --force \
               --git japaric/xargo \
               --tag $tag \
               --target x86_64-unknown-linux-musl

        rustup component list | grep 'rust-src.*installed' || \
            rustup component add rust-src

        rustup component list | grep 'rustfmt.*installed' || \
            rustup component add rustfmt-preview

        which cargo-bloat || cargo install cargo-bloat
}

main
