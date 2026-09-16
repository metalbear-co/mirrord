#!/bin/sh
#                                ░██       ░██
#                                          ░██
# ░██░████  ░███████   ░███████  ░██ ░████████  ░███████  ░████████   ░███████  ░██    ░██
# ░███     ░██    ░██ ░██        ░██░██    ░██ ░██    ░██ ░██    ░██ ░██    ░██ ░██    ░██
# ░██      ░█████████  ░███████  ░██░██    ░██ ░█████████ ░██    ░██ ░██        ░██    ░██
# ░██      ░██               ░██ ░██░██   ░███ ░██        ░██    ░██ ░██    ░██ ░██   ░███
# ░██       ░███████   ░███████  ░██ ░█████░██  ░███████  ░██    ░██  ░███████   ░█████░██
#                                                                                      ░██
#                                                                                ░███████
#
# > You've finished medschool... now what?
#
# A script for generating GitBook-ready files from mirrord config doc comments:
# 1. Runs medschool (requires rust toolchain to be set up) and reruns once upon failure
# 2. Splits the single file output into separate pages for each section beginning with a #Heading1
# 3. Adds YAML frontmatter with updated 'lastmod' date to each file
# 4. Outputs files in the TEMP_DIR_NAME directory
#
# Thing I learned writing this script: the macos version of csplit is wildly different to every other version!
# If youre on macos and suffering, try `brew install coreutils` and then invoke the gnu version with `gcsplit` (I also
# had to use `gecho` to stop macos echo from stripping out backslash characters from config examples, before I switched
# completely to `printf "%s"` because the GH runner had _another_ different version of `echo` 0_o)
#
# Note for future docs wranglers: since this script splits on #Heading1 lines, any new #Heading1s in the docs will
# completely break everything going on here. Make sure you update this script to accomodate for any new behaviour
# you want (new pages, etc.)

set -u
# ensure the temp dir exists
mkdir "$TEMP_DIR_NAME" 2> /dev/null
TEMP_FILE_NAME="temp.md"
TEMP_FILE_PATH=./$TEMP_DIR_NAME/$TEMP_FILE_NAME

set -e
# prep and split markdown from medschool
cargo run -p medschool -- --input ./mirrord/config/src --output "$TEMP_FILE_PATH"

cd "$TEMP_DIR_NAME"
# if csplit fails unexpectedly: read comment above (starting "Thing I learned")
printf "\nresidency: csplit on medschool:\n"
csplit -z -f docs_ -n 1 ./$TEMP_FILE_NAME '/^# /' '{*}'
rm ./$TEMP_FILE_NAME
printf "\nresidency: docs split successfully"

# add yaml frontmatter with current date
readmefrontmatter="---
title: Configuration Examples
date: 2023-05-17T12:59:39.000Z
lastmod: $(date +"%Y-%m-%dT00:00:00.000Z")
draft: false
images: []
menu:
  docs:
    parent: reference
weight: 110
toc: true
tags:
  - open source
  - team
  - enterprise
description: Getting started with mirrord configuration.
---
"
printf "%s\n" "$(echo "$readmefrontmatter"; cat docs_0)" > README.md
rm docs_0
printf "\nresidency: README.md updated"

optionsfrontmatter="---
title: Configuration Options
date: 2023-05-17T12:59:39.000Z
lastmod: $(date +"%Y-%m-%dT00:00:00.000Z")
draft: false
images: []
menu:
  docs:
    parent: reference
weight: 110
toc: true
tags:
  - open source
  - team
  - enterprise
description: >-
  Detailed documentation of all configuration options available for mirrord,
  including usage, defaults, and examples.
---
"
printf "%s\n" "$(echo "$optionsfrontmatter"; cat docs_1)" > options.md
rm docs_1
printf "\nresidency: options.md updated\n"
