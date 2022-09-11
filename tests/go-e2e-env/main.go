package main

import (
	"C"
	"fmt"
	"os"
)

func main() {
	if os.Getenv("MIRRORD_FAKE_VAR_FIRST") != "mirrord.is.running" {
		err := fmt.Errorf("missing env var MIRRORD_FAKE_VAR_FIRST")
		panic(err)
	}

	if os.Getenv("MIRRORD_FAKE_VAR_SECOND") != "7777" {
		err := fmt.Errorf("missing env var MIRRORD_FAKE_VAR_SECOND")
		panic(err)
	}

	if os.Getenv("MIRRORD_FAKE_VAR_THIRD") != "foo=bar" {
		err := fmt.Errorf("missing env var MIRRORD_FAKE_VAR_THIRD")
		panic(err)
	}
}
