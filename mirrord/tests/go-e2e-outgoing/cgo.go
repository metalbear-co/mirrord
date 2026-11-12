//go:build !windows

package main

// Importing C makes the app build with cgo, which links it dynamically, so that the mirrord layer
// can be preloaded into it. Windows builds leave this file out, because Bazel builds the apps for
// Windows without cgo, as it builds the layer tests' Go apps there. The empty line keeps this
// comment from being cgo's preamble, which cgo compiles as C.

import "C"
