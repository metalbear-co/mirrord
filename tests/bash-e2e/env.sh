#!/bin/bash

case $1 in
  include) first=1; second=0; third=0;;
  exclude) first=0; second=1; third=1;;
  *) first=1; second=1; third=1;;
esac

if [ $MIRRORD_FAKE_VAR_FIRST ]; then
  if [ $first == 0 ]; then
    echo "MIRRORD_FAKE_VAR_FIRST should not be set";

    exit -1
  fi

  if [[ $MIRRORD_FAKE_VAR_FIRST != "mirrord.is.running" ]]; then
    echo "MIRRORD_FAKE_VAR_FIRST wasn't of value mirrord.is.running";

    exit -1
  fi
elif [ $first == 1 ]; then
  echo "MIRRORD_FAKE_VAR_FIRST was not set";

  exit -1
fi

if [ $MIRRORD_FAKE_VAR_SECOND ]; then
  if [ $second == 0 ]; then
    echo "MIRRORD_FAKE_VAR_SECOND should not be set";

    exit -1
  fi

  if [[ $MIRRORD_FAKE_VAR_SECOND != "7777" ]]; then
    echo "MIRRORD_FAKE_VAR_SECOND wasn't of value 7777";

    exit -1
  fi
elif [ $second == 1 ]; then
  echo "MIRRORD_FAKE_VAR_SECOND was not set";

  exit -1
fi

if [ $MIRRORD_FAKE_VAR_THIRD ]; then
  if [ $third == 0 ]; then
    echo "MIRRORD_FAKE_VAR_THIRD should not be set";

    exit -1
  fi

  if [[ $MIRRORD_FAKE_VAR_THIRD != "foo=bar" ]]; then
    echo "MIRRORD_FAKE_VAR_THIRD wasn't of value foo=bar";

    exit -1
  fi
elif [ $third == 1 ]; then
  echo "MIRRORD_FAKE_VAR_THIRD was not set";

  exit -1
fi
