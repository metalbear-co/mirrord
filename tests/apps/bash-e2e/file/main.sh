#!/bin/bash

validation_file="/app/test.txt";
write_target="/tmp/bash_write_test.txt";
validation_text="Lorem ipsum dolor sit amet, consectetur adipiscing elit, sed do eiusmod tempor incididunt ut labore et dolore magna aliqua. Ut enim ad minim veniam, quis nostrud exercitation ullamco laboris nisi ut aliquip ex ea commodo consequat. Duis aute irure dolor in reprehenderit in voluptate velit esse cillum dolore eu fugiat nulla pariatur. Excepteur sint occaecat cupidatat non proident, sunt in culpa qui officia deserunt mollit anim id est laborum.";

case $1 in
  exists)
    if [[ ! -d "/app" ]]; then
        echo "Exists Directory operation [[ -d /app ]] failed";
        exit -1
    fi
    if [[ ! -r "$validation_file" ]]; then
        echo "Exists File operation [[ -r $validation_file ]] failed";
        exit -1
    fi
    # if [[ ! -f "$validation_file" ]]; then
    #     echo "Exists File operation [[ -f $validation_file ]] failed";
    #     exit -1
    # fi
    # if [[ ! -s "$validation_file" ]]; then
    #     echo "Exists File operation [[ -s $validation_file ]] failed";
    #     exit -1
    # fi
  ;;
  read)
      if [[ -r "$validation_file" ]]; then
        if [[ $(< $validation_file) != "$validation_text" ]]; then
          echo "Read File operation did not result in correct text";
          exit -1
        fi
      else
        echo "Read File operation did not find file $validation_file or is not radable";
        exit -1
      fi
    ;;
  write)
    if [[ -r "$validation_file" ]]; then
      echo "$validation_text" > "$write_target";

      if ! cmp -s "$write_target" "$validation_file"; then
        echo "Write File operation failed because $write_target was not equal to $validation_file";
        exit -1
      fi
      rm -f "$write_target" || exit 0;
    else
      echo "Write File operation could not file the compare file $validation_file";
      exit -1
    fi
    ;;
esac
