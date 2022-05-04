## [Unreleased]
## 2.0.0-alpha-3 - 1/5/2022
### Changed
* Change behavior of namespace change - set the namespace only in the packet sniffing, in a new thread so "command" socket will listen on the original network namespace
## 2.0.0-alpha-2 - 30/4/2022
### Fixed
* Fixed obtaining namespace & setting it using container id (seems to be a bug in new containerd-client version?)
### Misc
* Add manual image build and push workflow
* Add docker image build sanity
## 2.0.0-alpha-1 - 28/4/2022
### Misc
* Fix image build lacks cmake

## 2.0.0-alpha - 27/4/2022
### Refactor
* The code was refactored to support new upcoming features.
* The agent support multiple connections at once.
* Protocol is now binary for better performance (bincode).
* Changed the command line interface.
* Port and connections subscription is done when connecting.

## 1.0.1 - 10/3/2022
### Changed
* Update dependencies
* CI now builds from tag also.
## 1.0.0 - 21/02/2022
Initial release