"""Fails when the site in the directory of the argument has a source map, which would publish the
site's unminified source. `vite_site` in `vite.bzl` runs it for each site."""

import sys
from pathlib import Path


def main():
    (site,) = sys.argv[1:]
    # A path that isn't the site would have no source map either.
    if not Path(site, "index.html").is_file():
        sys.exit(f"{site} has no `index.html`, so it isn't a site that Vite built.")
    maps = sorted(str(path) for path in Path(site).rglob("*.map"))
    if maps:
        sys.exit("The site has source maps:\n" + "\n".join(maps))


main()
