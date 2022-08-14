if [ $MIRRORD_FAKE_VAR_FIRST ]; then
  if [[ $MIRRORD_FAKE_VAR_FIRST != "mirrord.is.running" ]]; then
    echo "MIRRORD_FAKE_VAR_FIRST wasn't of value mirrord.is.running";

    exit -1
  fi
else
  echo "MIRRORD_FAKE_VAR_FIRST was not set";

  exit -1
fi

if [ $MIRRORD_FAKE_VAR_SECOND ]; then
  if [[ $MIRRORD_FAKE_VAR_SECOND != "7777" ]]; then
    echo "MIRRORD_FAKE_VAR_SECOND wasn't of value 7777";

    exit -1
  fi
else
  echo "MIRRORD_FAKE_VAR_SECOND was not set";

  exit -1
fi
