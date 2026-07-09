Removed the experimental `go_cgo_stack_switch` flag. The Go 1.25+ cgo stack-switch fix is now covered only by the `go_asmcgocall` experimental flag.
`go_asmcgocall` is now enabled by default for OSS users.
