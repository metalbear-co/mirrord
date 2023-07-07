# Change Log

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/), and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

This project uses [*towncrier*](https://towncrier.readthedocs.io/) and the changes for the upcoming release can be found in <https://github.com/metalbear-co/mirrord/tree/main/changelog.d/>.

<!-- towncrier release notes start -->

## [3.50.0](https://github.com/metalbear-co/mirrord/tree/3.50.0) - 2023-07-07


### Removed

- Removed error capture error trace feature


### Added

- Add support for Argo rollout target.
  [#1593](https://github.com/metalbear-co/mirrord/issues/1593)


### Changed

- Agent container is no longer privileged. Instead, it is given a specific set
  of Linux capabilities: `CAP_NET_ADMIN`, `CAP_SYS_PTRACE`, `CAP_SYS_ADMIN`.
  [#1615](https://github.com/metalbear-co/mirrord/issues/1615)
- Changed agent job definition to include limits
  [#1621](https://github.com/metalbear-co/mirrord/issues/1621)


### Fixed

- Running java 17.0.6-tem with mirrord.
  [#1459](https://github.com/metalbear-co/mirrord/issues/1459)


### Internal

- Add support for installing operator with online license key.
- Cleaning kube resources after e2e tests no longer needs two runtime threads.


## [3.49.1](https://github.com/metalbear-co/mirrord/tree/3.49.1) - 2023-07-04


### Changed

- Small optimization in file reads to avoid sending empty data
  [#1254](https://github.com/metalbear-co/mirrord/issues/1254)
- Changed internal proxy to close after 5s of inactivity instead of 1
- use Frida's replace_fast in Linux Go hooks


### Fixed

- Child processes of python application would hang after a fork without an
  exec. [#1588](https://github.com/metalbear-co/mirrord/issues/1588)


### Internal

- Change locks in test process to be async to avoid deadlocks
- Fix listen ports flakiness by handling shutdown messages if they arrive
- Use Tokio current thread runtime in tests as it seems to be less flaky
- Use current thread Tokio runtime in fork integration test


## [3.49.0](https://github.com/metalbear-co/mirrord/tree/3.49.0) - 2023-07-03


### Added

- Added new analytics, see TELEMETRY.md for more details.


### Internal

- Fix some text in the operator documentation and progress reporting
- Remove IDE instructions from CONTRIBUTING.md


## [3.48.0](https://github.com/metalbear-co/mirrord/tree/3.48.0) - 2023-06-29


### Added

- Added Deployment to list of targets returnd from `mirrord ls`.
  [#1503](https://github.com/metalbear-co/mirrord/issues/1503)


### Changed

- Bump rust nightly to 2023-04-19 (latest nightly with support for const std
  traits). [#1457](https://github.com/metalbear-co/mirrord/issues/1457)
- Change loglevel of warnings to info of logs that were mistakenly warning
- Moved IntelliJ to its own repository and versioning


### Fixed

- Hook send_to and recv_from, leveraging our existing UDP interceptor mechanism
  to manually resolve DNS (as expected by netty, especially relevant for
  macos). [#1458](https://github.com/metalbear-co/mirrord/issues/1458)
- Add new rule to the OUTPUT chain of iptables in agent to support kubectl
  port-forward [#1479](https://github.com/metalbear-co/mirrord/issues/1479)
- If the local user application closes a socket but continues running, we now
  also stop mirroring/stealing from the target.
  [#1530](https://github.com/metalbear-co/mirrord/issues/1530)
- Add /home and /usr to the default file filter.
  [#1582](https://github.com/metalbear-co/mirrord/issues/1582)
- Fixed reporting EADDRINUSE as an error


### Internal

- (Operator only) Add `feature.network.incoming.on_concurrent_steal` option to
  allow overriding port locks.
- Improve medschool to produce more deterministic configuration.md, and
  (mostly) fixes it dropping some configuration docs during processing.
- Make mirrord ls deployment fetch parallel.
- Remove unused CRD for operator and don't error on missing operator
  credentials


## [3.47.0](https://github.com/metalbear-co/mirrord/tree/3.47.0) - 2023-06-20


### Added

- Added `listen_ports` to `incoming` config to control what port is actually
  being used locally
  so mirrored/stolen ports can still be accessed locally via those. If port
  provided by `listen_ports`
  isn't available, application will receive `EADDRINUSE`.
  Example configuration:
  ```json
  {
      "feature":
      {
          "incoming": {
              "listen_ports": [[80, 7111]]
          }
      }
  }
  ```
  will make port 80 available on 7111 locally, while stealing/mirroring port
  80. [#1554](https://github.com/metalbear-co/mirrord/issues/1554)


### Changed

- Changed the logic of choosing local port to use for intercepting mirror/steal
  sockets
  now instead of assigning a random port always, we try to use the original one
  and if we fail we assign random port.
  This only happens if `listen_ports` isn't used.
  [#1554](https://github.com/metalbear-co/mirrord/issues/1554)
- The path `/opt` itself is read locally by default (up until now paths inside
  that directory were read locally by default, but not the directory itself).
  [#1570](https://github.com/metalbear-co/mirrord/issues/1570)
- Changed back required IntelliJ version to 222+ from 223+
- Moved VSCode extension to its own repository and versioning
  https://github.com/metalbear-co/mirrord-vscode


### Fixed

- Running python with mirrord on apple CPUs.
  [#1570](https://github.com/metalbear-co/mirrord/issues/1570)


### Internal

- Use tagged version of ci-agent-build, so we can update Rust and the agent
  independently. [#1457](https://github.com/metalbear-co/mirrord/issues/1457)


## [3.46.0](https://github.com/metalbear-co/mirrord/tree/3.46.0) - 2023-06-14


### Added

- Add support for HTTP Path filtering
  [#1512](https://github.com/metalbear-co/mirrord/issues/1512)


### Changed

- Refactor vscode-ext code to be more modular


### Fixed

- Fixed bogus warnings in the VS Code extension.
  [#1504](https://github.com/metalbear-co/mirrord/issues/1504)
- Mirroring/stealing a port for a second time after the user application closed
  it once. [#1526](https://github.com/metalbear-co/mirrord/issues/1526)
- fixed using dotnet debugger on VSCode
  [#1529](https://github.com/metalbear-co/mirrord/issues/1529)
- Properly detecting and ignoring localhost port used by Rider's debugger.
- fix vscode SIP patch not working


### Internal

- Add a state Persistent Volume Claim to operator deployment setup.
- Bring the style guide into the repo.
- Fix vscode e2e job not running
- Remove OpenSSL dependency again
- Switch to new licensing and operator authenticaion flow.
- fix launch json for vscode extension
- fix macos build script to use directory's toolchain


## [3.45.2](https://github.com/metalbear-co/mirrord/tree/3.45.2) - 2023-06-12


### Internal

- Remove frida openSSL dependency


## [3.45.1](https://github.com/metalbear-co/mirrord/tree/3.45.1) - 2023-06-11


### Fixed

- Installation script now does not use `sudo` when not needed. This enbables
  installing mirrord in a `RUN` step in an ubuntu docker container, without
  installing `sudo` in an earlier step.
  [#1514](https://github.com/metalbear-co/mirrord/issues/1514)
- fix crio on openshift
  [#1534](https://github.com/metalbear-co/mirrord/issues/1534)
- Skipping `gcc` when debugging Go in VS Code extension.


### Internal

- change `mirrord-protocol` to have its own versioning. add `mirrord-macros`
  and `protocol_break` attribute to mark places we want to break on major
  updates.
  Add CI to verify that if protocol is changed, `Cargo.toml` is changed as well
  (to force bumps)
  Fix some of the structs being `OS` controlled, potentially breaking the
  protocol between different OS's.
  (`GetDEnts64(RemoteResult<GetDEnts64Response>),`)
  [#1355](https://github.com/metalbear-co/mirrord/issues/1355)
- Partial refactor towards 1512
  [#1512](https://github.com/metalbear-co/mirrord/issues/1512)
- Add integration test for DNS resolution
- Bumped versions of some VS Code extension dependencies.
- Frida bump and other dependencies
- Integration test for recv_from
- Reorganize dev docs
- Update our socket2 dependency, since the code we pushed there was released.


## [3.45.0](https://github.com/metalbear-co/mirrord/tree/3.45.0) - 2023-06-05


### Added

- Rider is now supported by the IntelliJ plugin.
  [#1012](https://github.com/metalbear-co/mirrord/issues/1012)


### Fixed

- Chagned agent to not return errors on reading from outgoing sockets, and
  layer to not crash in that case anyway


### Internal

- Use one thread for namespaced runtimes
  [#1287](https://github.com/metalbear-co/mirrord/issues/1287)
- Better timeformatting in e2e and maybe reduce flakiness?
- Fix nodejs deprecation warnings in CI
- Set MIRRORD_AGENT_IMAGE for vscode e2e


## [3.44.2](https://github.com/metalbear-co/mirrord/tree/3.44.2) - 2023-06-01


### Changed

- Change phrasing on version mismatch warning.
- Add `/Volumes` to default local on macOS
- Change Ping interval from 60s down to 30s.
- Changed local read defaults - list now includes `^/sbin(/|$)` and
  `^/var/run/com.apple`.


### Fixed

- Running postman with mirrord works.
  [#1445](https://github.com/metalbear-co/mirrord/issues/1445)
- Return valid error code when dns lookup fails, instead of -1.


### Internal

- Add E2E tests for vscode extension
  [#201](https://github.com/metalbear-co/mirrord/issues/201)
- Fixed flaky integration tests.
  [#1452](https://github.com/metalbear-co/mirrord/issues/1452)
- Fixed e2e tests' flakiness in the CI.
  [#1453](https://github.com/metalbear-co/mirrord/issues/1453)
- Change CI log level to be debug instead of trace
- Hooking `_NSGetExecutablePath` on macOS to strip the `mirrord-bin` temp dir
  off the path.
- Introduce a tool to extract config docs into a markdown file. Update docs to
  match whats in mirrord-dev.
- On macOS, if we path a binary for SIP and it is in a path that is inside a
  directory that has a name that ends with `.app`, we add the frameworks
  directory to `DYLD_FALLBACK_FRAMEWORK_PATH`.
- Provide buffer for `IndexAllocator` to avoid re-use of indices too fast


## [3.44.1](https://github.com/metalbear-co/mirrord/tree/3.44.1) - 2023-05-26


### Changed

- Never importing `RUST_LOG` environment variable from the cluster, regardless
  of configuration.


### Fixed

- Provide helpful error messages on errors in IDEs.
  [#1392](https://github.com/metalbear-co/mirrord/issues/1392)
- Log level control when running targetless.
  [#1446](https://github.com/metalbear-co/mirrord/issues/1446)
- Change to sticky balloon on warnings in intelliJ
  [#1456](https://github.com/metalbear-co/mirrord/issues/1456)
- Setting the namespace via the configuration was not possible in the IDE
  without also setting a target in the configuration file.
  [#1461](https://github.com/metalbear-co/mirrord/issues/1461)
- fixed IntelliJ failing silently when error happened on listing pods


### Internal

- Fix the test of reading from the SIP patch dir, that was not testing
  anything.
- Make the path field of `TargetConfig` an `Option`.


## [3.44.0](https://github.com/metalbear-co/mirrord/tree/3.44.0) - 2023-05-24


### Added

- Changed agent's pause feature. Now the pause is requested dynamically by CLI
  during setup and the agent keeps the target container paused until exit or
  the unpause request was received.
  [#1408](https://github.com/metalbear-co/mirrord/issues/1408)
- Added support for NPM run configuration on JetBrains products.
  [#1418](https://github.com/metalbear-co/mirrord/issues/1418)


### Changed

- Change mirrord ls to show only pods that are in running state (not
  crashing,starting,etc)
  [#1436](https://github.com/metalbear-co/mirrord/issues/1436)
- Change fs mode to be local with overrides when targetless is used
- Make progress text consitently lowercase.


### Fixed

- Fix misalignment on IntelliJ not accepting complex path in target
  [#1441](https://github.com/metalbear-co/mirrord/issues/1441)
- Add impersonate permissions for GCP specific RBAC in operator


### Internal

- Fix node spawn test flakiness on macOS
  [#1431](https://github.com/metalbear-co/mirrord/issues/1431)


## [3.43.0](https://github.com/metalbear-co/mirrord/tree/3.43.0) - 2023-05-22


### Added

- Support for targetless execution: when not specifying any target for the
  agent, mirrord now spins up an independent agent. This can be useful e.g. if
  you are just interested in getting the cluster's DNS resolution and outgoing
  connectivity but don't want any pod's incoming traffic or FS.
  [#574](https://github.com/metalbear-co/mirrord/issues/574)
- Support for targetless mode in IntelliJ based IDEs.
  [#1375](https://github.com/metalbear-co/mirrord/issues/1375)
- Support for targetless mode in vscode.
  [#1376](https://github.com/metalbear-co/mirrord/issues/1376)


### Changed

- If a user application tries to read paths inside mirrord's temp dir, we hook
  that and read the path outside instead.
  [#1403](https://github.com/metalbear-co/mirrord/issues/1403)
- Don't print error if we fail checking for operator


### Fixed

- Added better detection for protected binaries, fixes not loading into Go
  binary [#1397](https://github.com/metalbear-co/mirrord/issues/1397)
- Disallow binding on the same address:port twice. Solves part of issue 1123.
  [#1123](https://github.com/metalbear-co/mirrord/issues/1123)
- Fix the lost update bug with config dropdown for intelliJ
  Fix the lost update bug with config dropdown for intelliJ.
  [#1420](https://github.com/metalbear-co/mirrord/issues/1420)
- Fix intelliJ compatability issue by implementing missing
  createPopupActionGroup


### Internal

- Run IntelliJ Plugin Verifier on CI
  [#1417](https://github.com/metalbear-co/mirrord/issues/1417)
- Remove bors.toml since we use GH merge queue now
- Upgrade k8s dependencies and rustls, remove ugly feature ip patch


## [3.42.0](https://github.com/metalbear-co/mirrord/tree/3.42.0) - 2023-05-15


### Added

- mirrord config dropdown for intelliJ.
  [#1030](https://github.com/metalbear-co/mirrord/issues/1030)
- Log agent version when initializing the agent.


### Changed

- Remove quotes in InvalidTarget' target error message


### Fixed

- Use ProgressManager for mirrord progress on intelliJ
  [#1337](https://github.com/metalbear-co/mirrord/issues/1337)
- Fixed `go run` failing because of reading remote files by maing paths under
  /private and /var/folders read locally by default.
  [#1397](https://github.com/metalbear-co/mirrord/issues/1397)
- Fix not loading into Go because of SIP by adding into default patched
  binaries


## [3.41.1](https://github.com/metalbear-co/mirrord/tree/3.41.1) - 2023-05-07


### Fixed

- Fixed regression in GoLand and NodeJS causing a crash
  [#1389](https://github.com/metalbear-co/mirrord/issues/1389)


## [3.41.0](https://github.com/metalbear-co/mirrord/tree/3.41.0) - 2023-05-06


### Added

- Last selected target is now remembered in IntelliJ extension and shown first
  in the target selection dialog.
  [#1347](https://github.com/metalbear-co/mirrord/issues/1347)
- Warn user when their mirrord version doesn't match the operator version.


### Changed

- mirrord loading progress is displayed in the staus indicator on IntelliJ,
  replacing the singleton notifier
  [#1337](https://github.com/metalbear-co/mirrord/issues/1337)


### Fixed

- Fix crash on unexpected LogMessage
  [#1380](https://github.com/metalbear-co/mirrord/issues/1380)
- Added hook for recvfrom to support cases where caller expects the messages to
  be from address they were sent to.
  [#1386](https://github.com/metalbear-co/mirrord/issues/1386)


### Internal

- Add x-session-id to operator request, that is persistent across child
  processes in a single mirrord exec.
- Improve metadata for VSCode extension
- Remove unnecessary DNS resolve on agent addr when incluster feature is
  enabled in mirrord-kube.


## [3.40.0](https://github.com/metalbear-co/mirrord/tree/3.40.0) - 2023-05-01


### Added

- Add a message informing users of the operator when they impersonate
  deployments with mirrord.
  [#add-operator-message](https://github.com/metalbear-co/mirrord/issues/add-operator-message)
- Last selected target is now remembered in VS Code and shown first in the
  quick pick widget.
  [#1348](https://github.com/metalbear-co/mirrord/issues/1348)


### Fixed

- PyCharm plugin now detects `pydevd` debugger and properly excludes its port.
  [#1020](https://github.com/metalbear-co/mirrord/issues/1020)
- VS Code extension now detects `debugpy` debugger and properly excludes its
  port. [#1145](https://github.com/metalbear-co/mirrord/issues/1145)
- Fixed delve patch not working on GoLand macOS when running go tests
  [#1364](https://github.com/metalbear-co/mirrord/issues/1364)
- Fixed issues when importing some packages in Python caused by PYTHONPATH to
  be used from the remote pod (add it to exclude)


### Internal

- Added Clippy lint for slicing and indexing.
  [#1049](https://github.com/metalbear-co/mirrord/issues/1049)
- Eliminate unused variable warnings for E2E tests on macOS.


## [3.39.1](https://github.com/metalbear-co/mirrord/tree/3.39.1) - 2023-04-21


### Changed

- Updated IntelliJ usage gif.


### Fixed

- Add magic fix (by polling send_request) to (connection was not ready) hyper
  error. Also adds some more logs around HTTP stealer.
  [#1302](https://github.com/metalbear-co/mirrord/issues/1302)


### Internal

- Fix arduino/setup-protoc rate limiting error.


## [3.39.0](https://github.com/metalbear-co/mirrord/tree/3.39.0) - 2023-04-19


### Added

- Support for Node.js on IntelliJ - run/debug JavaScript scripts on IntelliJ
  with mirrord. [#1284](https://github.com/metalbear-co/mirrord/issues/1284)


### Fixed

- Use RemoteFile ops in gethostname to not have a local fd.
  [#1202](https://github.com/metalbear-co/mirrord/issues/1202)


### Internal

- Fix latest tag
- Project build instructions in the testing guide now include the protoc
  dependency.


## [3.38.1](https://github.com/metalbear-co/mirrord/tree/3.38.1) - 2023-04-19


### Fixed

- Release action should work now.

### Internal

- Add protobuf-compiler to rust docs action

## [3.38.0](https://github.com/metalbear-co/mirrord/tree/3.38.0) - 2023-04-18


### Added

- Add support for cri-o container runtime.
  [#1258](https://github.com/metalbear-co/mirrord/issues/1258)
- A descriptive message is now presented in the IntelliJ extension when no
  target is available. Listing targets failure is now handled and an error
  notification is presented.
  [#1267](https://github.com/metalbear-co/mirrord/issues/1267)
- Added waitlist registration via cli.
  Join the waitlist to try out first mirrord for Teams which is invite only at
  the moment. [#1303](https://github.com/metalbear-co/mirrord/issues/1303)
- Add email option to help messages.
  [#1318](https://github.com/metalbear-co/mirrord/issues/1318)


### Changed

- When patching for SIP, use arm64 if possible (running on aarch64 and an arm64
  binary is available).
  [#1155](https://github.com/metalbear-co/mirrord/issues/1155)
- Changed our Discord invite link to https://discord.gg/metalbear


### Fixed

- Change detour bypass to be more robust, not crashing in case it can't update
  the bypass [#1320](https://github.com/metalbear-co/mirrord/issues/1320)


### Internal

- Added integration tests for outgoing UDP and TCP.
  [#1051](https://github.com/metalbear-co/mirrord/issues/1051)
- All Kubernetes resources are now deleted after E2E tests. Use
  `MIRRORD_E2E_PRESERVE_FAILED` environment variable to preserve resources from
  failed tests. All resources created for E2E tests now share a constant label
  `mirrord-e2e-test-resource=true`.
  [#1256](https://github.com/metalbear-co/mirrord/issues/1256)
- Added a debugging guide for the IntelliJ extension.
  [#1278](https://github.com/metalbear-co/mirrord/issues/1278)
- Add `impersonate` permission on `userextras/accesskeyid`, `userextras/arn`,
  `userextras/canonicalarn` and `userextras/sessionname` resources to operator
  setup.
- Sometimes when using console logger mirrord crashes since tokio runtime isn't
  initialized, changed to just use a thread


## [3.37.0](https://github.com/metalbear-co/mirrord/tree/3.37.0) - 2023-04-14


### Removed

- Removed armv7 builds that were wrongly added


### Added

- Add `ignore_ports` to `incoming` configuration so you can have ports that
  only listen
  locally (mirrord will not steal/mirror those ports).
  [#1295](https://github.com/metalbear-co/mirrord/issues/1295)
- Add support for `xstatfs` to prevent unexpected behavior with SQLite.
  [#1270](https://github.com/metalbear-co/mirrord/issues/1270)


### Changed

- Improved bad target error
  [#1291](https://github.com/metalbear-co/mirrord/issues/1291)


### Internal

- Optimize agent Dockerfile for better cache use
  [#1280](https://github.com/metalbear-co/mirrord/issues/1280)
- Cover more areas of the code and targets using clippy in CI and fix its
  warnings
- Rely more on Rusts own async trait and drop async-trait crate (the agent cant
  fully switch yet though).
  [#use-rust-async-traits](https://github.com/metalbear-co/mirrord/issues/use-rust-async-traits)


## [3.36.0](https://github.com/metalbear-co/mirrord/tree/3.36.0) - 2023-04-13


### Added

- Notify clients about errors happening in agent's background tasks.
  [#1163](https://github.com/metalbear-co/mirrord/issues/1163)
- Add support for the imagePullSecrets parameter on the agent pod. This can be
  specified in the configuration file, under agent.image_pull_secrets.
  [#1276](https://github.com/metalbear-co/mirrord/issues/1276)


### Internal

- Fix pause E2E test.
  [#1261](https://github.com/metalbear-co/mirrord/issues/1261)


## [3.35.0](https://github.com/metalbear-co/mirrord/tree/3.35.0) - 2023-04-11


### Added

- Added an error prompt to the VS Code extension when there is no available
  target in the configured namespace.
  [#1266](https://github.com/metalbear-co/mirrord/issues/1266)


### Changed

- HTTP traffic stealer now supports HTTP/2 requests.
  [#922](https://github.com/metalbear-co/mirrord/issues/922)


### Fixed

- Executable field was set to null if present, but no SIP patching was done.
  [#1271](https://github.com/metalbear-co/mirrord/issues/1271)
- Fixed random crash in `close_layer_fd` caused by supposed closing of
  stdout/stderr then calling to log that writes to it


### Internal

- Use DashMap for `OPEN_DIRS`
  [#1240](https://github.com/metalbear-co/mirrord/issues/1240)
- Use DashMap for `MANAGED_ADDRINFO`
  [#1241](https://github.com/metalbear-co/mirrord/issues/1241)
- Use DashMap for `ConnectionQueue`
  [#1242](https://github.com/metalbear-co/mirrord/issues/1242)
- Implemented `Default` for `Subscriptions`. Replaced usages of
  `Subscriptions::new` with `Default::default`.
- Improve testing guide.
- Removed unnecessary trait bounds for `Default` implementation on
  `IndexAllocator`. Replaced usages of `IndexAllocator::new` with
  `Default::default`.
- Update contributing guide.
- Update testing and building docs, and add instructions for the IDE
  extensions.


## [3.34.0](https://github.com/metalbear-co/mirrord/tree/3.34.0) - 2023-03-30


### Added

- Support for running SIP binaries via the vscode extension, for common
  configuration types.
  [#1061](https://github.com/metalbear-co/mirrord/issues/1061)


### Changed

- Add the failed connection address on failure to debug easily
- New IntelliJ icons - feel free to give feedback


### Fixed

- Fix internal proxy receiving signals from terminal targeted for the mirrord
  process/parent process by using setsid
  [#1232](https://github.com/metalbear-co/mirrord/issues/1232)
- fix listing pods failing when config file exists on macOS
  [#1245](https://github.com/metalbear-co/mirrord/issues/1245)


### Internal

- Use DashMap instead of Mutex<HashMap> for `SOCKETS`
  [#1239](https://github.com/metalbear-co/mirrord/issues/1239)
- Some small changes to make building the JetBrains plugin locally simpler.
- Update IntelliJ dependencies
- Update dependencies
- Update rust and remove unneccessary feature.


## [3.33.1](https://github.com/metalbear-co/mirrord/tree/3.33.1) - 2023-03-28


### Changed

- Add default requests and limits values to mirrord-operator setup
  (100m/100Mi).


### Fixed

- Change CLI's version update message to display the correct command when
  mirrord has been installed with homebrew.
  [#1194](https://github.com/metalbear-co/mirrord/issues/1194)
- fix using config with WSL on JetBrains
  [#1210](https://github.com/metalbear-co/mirrord/issues/1210)
- Fix internal proxy exiting before IntelliJ connects to it in some situations
  (maven). Issue was parent process closing causing child to exit. Fixed by
  waiting from the extension call to the child.
  [#1211](https://github.com/metalbear-co/mirrord/issues/1211)
- mirrord-cli: update cli so failing to use operator will fallback to
  no-operator mode.
  [#1218](https://github.com/metalbear-co/mirrord/issues/1218)
- Add option to install specific version using the `install.sh` script via
  command line argument or `VERSION` environment variable
  [#1222](https://github.com/metalbear-co/mirrord/issues/1222)
- Change connection reset to be a trace message instead of error
- Error when agent exits.


### Internal

- Bring the testing documentation into the repo, link it in readme, and add
  some information.
- Introduce CheckedInto trait to convert raw pointers (checking for null) in
  Detour values.
  [#detours](https://github.com/metalbear-co/mirrord/issues/detours)
- Re-enable http mirror e2e tests..
  [#947](https://github.com/metalbear-co/mirrord/issues/947)
- Change OPEN_FILES from Mutex HashMap to just using DashMap.
  [#1206](https://github.com/metalbear-co/mirrord/issues/1206)
- Refactor file ops open/read/close to allow us to directly manipulate the
  remote file (in agent) withouht going through C (mainly used to not leak the
  remote file due to how gethostname works).

  Change dup to take an argument that signals if we should change the fd from
  SOCKETS to OPEN_FILES (or vice-versa).
  [#1202](https://github.com/metalbear-co/mirrord/issues/1202)




## [3.33.0](https://github.com/metalbear-co/mirrord/tree/3.33.0) - 2023-03-22


### Added

- Support for outgoing unix stream sockets (configurable via config file or
  environment variable).
  [#1105](https://github.com/metalbear-co/mirrord/issues/1105)
- Add  version of hooked functions.
  [#1203](https://github.com/metalbear-co/mirrord/issues/1203)


### Changed

- add `Hash` trait on `mirrord_operator::license::License` struct
- dependencies bump and cleanup
- fix mirrord loading twice (to build also) and improve error message when no
  pods found


### Fixed

- fix f-stream functions by removing its hooks and add missing underlying libc
  calls [#947](https://github.com/metalbear-co/mirrord/issues/947)
- fix deadlock in go20 test (remove trace?)
  [#1206](https://github.com/metalbear-co/mirrord/issues/1206)


### Internal

- set timeout for flaky/hanging test


## [3.32.3](https://github.com/metalbear-co/mirrord/tree/3.32.3) - 2023-03-19


### Changed

- change outgoing connection drop to be trace instead of error since it's not
  an error


### Fixed

- Support stealing on meshed services with ports specified in
  --skip-inbound-ports on linkerd and itsio equivalent.
  [#1041](https://github.com/metalbear-co/mirrord/issues/1041)


## [3.32.2](https://github.com/metalbear-co/mirrord/tree/3.32.2) - 2023-03-14


### Fixed

- fix microk8s support by adding possible containerd socket path
  [#1186](https://github.com/metalbear-co/mirrord/issues/1186)
- fix gethostname null termination missing
  [#1189](https://github.com/metalbear-co/mirrord/issues/1189)
- Update webbrowser dependency to fix security issue.


## [3.32.1](https://github.com/metalbear-co/mirrord/tree/3.32.1) - 2023-03-12


### Fixed

- fix mirroring not handling big requests - increase buffer size (in rawsocket
  dependency).
  also trace logs to not log the data.
  [#1178](https://github.com/metalbear-co/mirrord/issues/1178)
- fix environment regression by mixing the two approaches together.
  priority is proc > oci (via container api)
  [#1180](https://github.com/metalbear-co/mirrord/issues/1180)


### Internal

- compile/test speed improvements

  1. add CARGO_NET_GIT_FETCH_WITH_CLI=true to agent's Dockerfile since we found
  out it
      saves a lot of time on fetching (around takes 60s when using libgit2)
  2. change `rust-toolchain.toml` so it won't auto install unneeded targets
  always
  3. remove `toolchain: nightly` parameter from `actions-rs/toolchain@v1` since
  it's
      not needed because we have `rust-toolchain.toml`
      saves a lot of time on fetching (takes around 60s when using libgit2)
  4. switch to use `actions-rust-lang/setup-rust-toolchain@v1` instead of
  `actions-rs/toolchain@v1`
      since it's deprecated and doesn't support `rust-toolchain.toml`
  5. remove s`Swatinem/rust-cache@v2` since it's included in
  `actions-rust-lang/setup-rust-toolchain@v1`
  6. use latest version of `Apple-Actions/import-codesign-certs` to remove
  warnings
- print logs of stealer/sniffer start failure
- run docker/containerd runtime at the same time to make e2e faster
- use base images for agent to reduce build time


## [3.32.0](https://github.com/metalbear-co/mirrord/tree/3.32.0) - 2023-03-08


### Changed

- mirrord-layer: changed result of `getsockname` to return requested socket on
  `bind` instead of the detoured socket address
  [#1047](https://github.com/metalbear-co/mirrord/issues/1047)
- mirrord-layer: Added `SocketId` to `UserSocket` as a better way of
  identifying sockets, part of #1054.
  [#1054](https://github.com/metalbear-co/mirrord/issues/1054)
- CHANGELOG - changed to use towncrier
- Change socket error on reading from outgoing sockets and mirror to be info
  instead of error


### Fixed

- Possible bug when bound address is bypassed and socket stays in `SOCKETS`
  map.


### Internal

- Change release.yaml so pushing final tags will occur only on real releases
  while manual releases will push into `ghcr.io/metalbear-co/mirrord-staging:
  ${{ github.sha }}`
  so we can leverage github CI for testing images.
- Don't build builder image as part of the build, use a prebuilt image -
  improve cd time
  Use `taiki-e/install-action` instead of `cargo install` (compiles from
  source) for installing `cross`.
- Fix broken aarch build


## 3.31.0

### Added

- config: `ignore_localhost` to `outgoing` config for ignoring localhost connections, meaning it will connect to local
  instead of remote localhost.
- config: `ignore_localhost` to `incoming` config for ignoring localhost bound sockets, meaning it will not steal/mirror those.
- combination of `ignore_localhost` in `incoming` and `outgoing` can be useful when you run complex processes that does
  IPC over localhost.
- `sip_binaries` to config file to allow specifying SIP-protected binaries that needs to be patched
  when mirrord doesn't detect those. See [#1152](https://github.com/metalbear-co/mirrord/issues/1152).

### Fixed

- Unnecessary error logs when running a script that uses `env` in its shebang.
- VSCode extension: running Python script with debugger fails because it tries to connect to the debugger port remotely.
- Big file leading to timeout: we found out that `bincode` doesn't do so well with large chunked messages
  so we limited remote read size to 1 megabyte, and read operation supports getting partial data until EOF.
- mirrord-operator: fix silent fail when parsing websocket messages fails.

### Changed

- improved mirrord cli help message.
- mirrord-config: Change `flush_connections` default to `true`, related to
  [#1029](https://github.com/metalbear-co/mirrord/issues/1029).

## 3.30.0

### Added

- mirrord-layer: Added `port_mapping` under `incoming` configuration to allow mapping local ports to custom
  remote port, for example you can listen on port 9999 locally and it will steal/mirror
  the remote 80 port if `port_mapping: [[9999, 80]]`. See [#1129](https://github.com/metalbear-co/mirrord/issues/1129)

### Fixed

- Fix issue when two (or more) containerd sockets exist and we use the wrong one. Fixes [#1133](https://github.com/metalbear-co/mirrord/issues/1133).
- Invalid toml in environment variables configuration examples.

### Changed

- Use container's runtime env instead of reading it from `/proc/{container_root_pid}/environ` as some processes (such as nginx) wipe it. Fixes [#1135](https://github.com/metalbear-co/mirrord/issues/1135)
- Removed the prefix "test" from all test names - [#1065](https://github.com/metalbear-co/mirrord/issues/1065).
- Created symbolic link from the vscode directory to the `LICENSE` and `CHANGELOG.md` files so that mirrord developers
  don't need to copy them there before building the app.
- mirrord-layer: `socket` hook will now block ipv6 requests and will return EAFNOSUPPORT. See [#1121](https://github.com/metalbear-co/mirrord/issues/1121).

## 3.29.0

### Added

- mirrord debug feature (for mirrord developers to debug mirrord): Cause the agent to exit early with an error.
- mirrord E2E tests: support for custom namespaces.

### Fixed

- Unpause the target container before exiting if the agent exits early on an error and the container is paused -
   [#1111](https://github.com/metalbear-co/mirrord/issues/1111).
- intellij-plugin: fix issue where execution hangs when running using Gradle. Fixes [#1120](https://github.com/metalbear-co/mirrord/issues/1120).
- intellij-plugin: fix issue where mirrord doesn't load into gradle, was found when fixing [#1120](https://github.com/metalbear-co/mirrord/issues/1120).
- mirrord-agent: reintroduce `-o lo` back to iptable rules to prevent issue where outinging messags could be intersepted by mirrord as incoming ones.
- mirrord-layer: binding same port on different IPs leads to a crash due to `ListenAlreadyExists` error.
  This is now ignored with a `info` message since we can't know if the IP/Port was already bound
  or not. Created a follow up issue to complete implementation and error at application's bind.

## 3.28.4

### Fixed

- VSCode Extension: Fix wrong CLI path on Linux

## 3.28.3

### Fixed

- VSCode Extension: Fix wrong CLI path

## 3.28.2

### Fixed

- Fix error in VSCode extension compilation

## 3.28.1

### Fixed

- CI: fix error caused by missing dir

## 3.28.0

### Changed

- Change VSCode extension to package all binaries and select the correct one based on the platform. Fixes [#1101](https://github.com/metalbear-co/mirrord/issues/1101).
- agent: add log to error when handling a client message fails.

### Fixed

- agent: Make sniffer optional to support cases when it's not available and mirroring is not required.

## 3.27.1

### Changed

- Update operator version

## 3.27.0

### Fixed

- mirrord now handles it when the local app closes a forwarded stolen tcp connection instead of exiting with an error. Potential fix for [#1063](https://github.com/metalbear-co/mirrord/issues/1063).
- missing kubeconfig doesn't fail extensions (it failed because it first tried to resolve the default then used custom one)

### Changed

- layer: Don't print error when tcp socket faces error as it can be a normal flow.
- internal proxy - set different timeout for `mirrord exec` and running from extension
  fixing race conditions when running from IntelliJ/VSCode.
- Changed `with_span_events` from `FmtSpan::Active` to `FmtSpan::NEW | FmtSpan::CLOSE`.
  Practically this means we will have less logs on enter/exit to span and only when it's first created
  and when it's closed.
- JetBrains Plugin: Add debug logs for investigating user issues.
- JetBrains compatability: set limit from 222 (2022.2.4) since 221 isn't supported by us.
- Make `kubeconfig` setting effective always by using `-f` in `mirrord ls`.
- mirrord agent can now run without sniffer, will not be able to mirror but can still steal.
  this is to enable users who have older kernel (4.20>=) to use the steal feature.

## 3.26.1

### Fixed

- VSCode Extension: Prevent double prompting of the user to select the target if not specified in config. See [#1080](https://github.com/metalbear-co/mirrord/issues/1080).

### Changed

- JetBrains enable support from 2021.3 (like we had in 3.14.3).

## 3.26.0

### Changed

- mirrord-agent: localhost traffic (like healthprobes) won't be stolen by mirrord on meshed targets to allign behavior with non meshed targets. See [#1070](https://github.com/metalbear-co/mirrord/pull/1070)
- Filter out agent pods from `mirrord ls`, for better IDE UX. Closes [#1045](https://github.com/metalbear-co/mirrord/issues/1045).
- Not exiting on SIP-check fail. Instead, logging an error and letting the program fail as it would without mirrord.
  See [#951](https://github.com/metalbear-co/mirrord/issues/951).

### Fixed

- Fix cache does not work on test-agent workflow. See [#251](https://github.com/metalbear-co/mirrord/issues/251).
- CI: merge queue + branch protection issues

## 3.25.0

### Added

- `gethostname` detour that returns contents of `/etc/hostname` from target pod. See relevant [#1041](https://github.com/metalbear-co/mirrord/issues/1041).

### Fixed

- `getsockname` now returns the **remote** local address of the socket, instead of the
  **local fake** address of the socket.
  This should fix issues with Akka or other software that checks the local address and
  expects it to match the **local ip of the pod**.
  This breaks agent protocol (agent/layer need to match).
- GoLand debug fails because of reading `/private/var/folders` remotely (trying to access self file?). fixed with filter change (see below)

### Changed

- VSCode extension: update dialog message
- JetBrains: can now change focus from search field to targets using tab/shift+tab (for backwrad)
- Refactor - mirrord cli now spawns `internal proxy` which does the Kubernetes operations for
  the layer, so layer need not interact with k8s (solves issues with remote/local env mix)
- filter: add `/private/var/folders" to default local read override
- filter: fixed regex for `/tmp` default local read override
- disable flask e2e until we solve the glibc issue (probably fstream issue)

## 3.24.0

### Added

- Add a field to mirrord-config to specify custom path for kubeconfig , resolves [#1027](https://github.com/metalbear-co/mirrord/issues/1027).

### Changed

- Removed limit on future builds `untilBuild` in JetBrains plugin.
- IntelliJ-ext: change the dialog to provide a sorted list and make it searchable, resolves [#1031](https://github.com/metalbear-co/mirrord/issues/1031).
- mirrord-layer: Changed default to read AWS credentials + config from remote pod.
- Removed unused env var (`MIRRORD_EXTERNAL_ENV`)
- mirrord-agent: Use `conntrack` to flush stealer connections (temporary fix for
  [#1029](https://github.com/metalbear-co/mirrord/issues/1029)).

### Fixed

- Added env guard to be used in cli + extension to prevent (self) misconfigurations (our kube settings being used from remote).

## 3.23.0

### Fixed

- mirrord-config: Fix disabled feature for env in config file, `env = false` should work. See [#1015](https://github.com/metalbear-co/mirrord/issues/1015).
- VS Code extension: release universal extension as a fallback for Windows and other platforms to be used with WSL/Remote development. Fixes [#1017](https://github.com/metalbear-co/mirrord/issues/1017)
- Fix `MIRRORD_AGENT_RUST_LOG` can't be more than info due to dependency on info log.
- Fix pause feature not working in extension due to writing to stdout (changed to use trace)

### Changed

- `DNSLookup` failures changed to be info log from error since it is a common case.
- mirrord-agent: now prints "agent ready" instead of logging it so it can't be fudged with `RUST_LOG` control.
- mirrord-agent: `agent::layer_recv` changed instrumentation to be trace instead of info.
- mirrord-layer/agent: change ttl of job to be 1 second for cases where 0 means in cluster don't clean up.
- Convert go fileops e2e tests into integration tests. Part of
  [#994](https://github.com/metalbear-co/mirrord/issues/994#issuecomment-1410721960).

## 3.22.0

### Changed

- Rust: update rust toolchain (and agent rust `DOCKERFILE`) to `nightly-2023-01-31`.
- exec/spawn detour refactor [#999](https://github.com/metalbear-co/mirrord/issues/999).
- mirrord-layer: Partialy load mirrord on certian processes that spawn other processes to allow sip patch on the spawned process.
  This to prevent breaking mirrord-layer load if parent process is specified in `--skip-processes`.  (macOS only)

### Fixed

- mirrord-layer: DNS resolving doesn't work when having a non-OS resolver (using UDP sockets)
  since `/etc/resolv.conf` and `/etc/hosts` were in the local read override,
  leading to use the local nameserver for resolving. Fixes [#989](https://github.com/metalbear-co/mirrord/issues/989)
- mirrord-agent: Infinite reading a file when using `fgets`/`read_line` due to bug seeking to start of file.
- Rare deadlock on file close that caused the e2e file-ops test to sometimes fail
  ([#994](https://github.com/metalbear-co/mirrord/issues/994)).

## 3.21.0

### Added

- Support for Go's `os.ReadDir` on Linux (by hooking the `getdents64` syscall). Part of
  [#120](https://github.com/metalbear-co/mirrord/issues/120).
- Test mirrord with Go 1.20rc3.

### Changed

- mirrord-agent: Wrap agent with a parent proccess to doublecheck the clearing of iptables. See [#955](https://github.com/metalbear-co/mirrord/issues/955)
- mirrord-layer: Change `HOOK_SENDER` from `Option` to `OnceLock`.

### Fixed

- mirrord-agent: Handle HTTP upgrade requests when the stealer feature is enabled
  (with HTTP traffic) PR [#973](https://github.com/metalbear-co/mirrord/pull/973).
- E2E tests compile on MacOS.
- mirrord could not load into some newer binaries of node -
  [#987](https://github.com/metalbear-co/mirrord/issues/987). Now hooking also `posix_spawn`, since node now uses
  `libuv`'s `uv_spawn` (which in turn calls `posix_spawn`) instead of libc's `execvp` (which calls `execve`).
- Read files from the temp dir (defined by the system's `TMPDIR`) locally, closes
  [#986](https://github.com/metalbear-co/mirrord/issues/986).

## 3.20.0

### Added

- Support impersonation in operator

### Fixed

- Go crash in some scenarios [#834](https://github.com/metalbear-co/mirrord/issues/834).
- Remove already deprecated `--no-fs` and `--rw` options, that do not do anything anymore, but were still listed in the
  help message.
- Bug: SIP would fail the second time to run scripts for which the user does not have write permissions.

### Changed

- Change layer/cli logs to be to stderr instead of stdout to avoid mixing with the output of the application. Closes [#786](https://github.com/metalbear-co/mirrord/issues/786)

## 3.19.2

### Changed

- Code refactor: moved all file request and response types into own file.

## 3.19.1

### Fixed

- Changelog error failing the JetBrains release.

## 3.19.0

### Changed

- mirrord-operator: replace operator api to use KubernetesAPI extension. [#915](https://github.com/metalbear-co/mirrord/pull/915)

### Fixed

- tests: flaky passthrough fix. Avoid 2 agents running at the same time, add minimal sleep (1s)
- macOS x64/SIP(arm): fix double hooking `fstatat$INODE64`. Possible crash and undefined behavior.

### Added

- introduce `mirrord-console` - a utility to debug and investigate mirrord issues.

### Deprecated

- Remove old fs mode
  - cli: no `--rw` or `--no-fs`.
  - layer: no `MIRRORD_FILE_OPS`/`MIRRORD_FILE_RO_OPS`/`MIRRORD_FILE_FILTER_INCLUDE`/`MIRRORD_FILE_FILTER_EXCLUDE`

## 3.18.2

### Fixed

- crash when `getaddrinfo` is bypassed and libc tries to free our structure. Closes [#930](https://github.com/metalbear-co/mirrord/issues/930)
- Stealer hangs on short streams left open and fails on short closed streams to filtered HTTP ports -
 [#926](https://github.com/metalbear-co/mirrord/issues/926).

## 3.18.1

### Fixed

- Issue when connect returns `libc::EINTR` or `libc::EINPROGRESS` causing outgoing connections to fail.
- config: file config updated to fix simple pattern of IncomingConfig. [#933](https://github.com/metalbear-co/mirrord/pull/933)

## 3.18.0

### Added

- Agent now sends error encountered back to layer for better UX when bad times happen. (This only applies to error happening on connection-level).
- Partial ls flow for Go on macOS (implemented `fdopendir` and `readdir_r`). Closes [#902](https://github.com/metalbear-co/mirrord/issues/902)
- New feature: HTTP traffic filter!
  - Allows the user to steal HTTP traffic based on HTTP request headers, for example `Client: me` would steal requests that match this header,
    while letting unmatched requests (and non-HTTP packets) through to their original destinations.

### Fixed

- Update the setup-qemu-action action to remove a deprecation warning in the Release Workflow
- stat functions now support directories.
- Possible bugs with fds being closed before time (we now handle dup'ing of fds, and hold those as ref counts)

### Changed

- agent: Return better error message when failing to use `PACKET_IGNORE_OUTGOING` flag.

## 3.17.0

### Added

- Add brew command to README

### Fixed

- intellij plugin: mirrord icon should always load now.
- intellij plugin: on target selection cancel, don't show error - just disable mirrord for the run and show message.
- fixed setting a breakpoint in GoLand on simple app hanging on release build (disabled lto). - Fixes [#906](https://github.com/metalbear-co/mirrord/issues/906).

### Deprecated

- Removed `disabled` in favor of `local` in `fs` configuration.

### Changed

- update `kube` dependency + bump other
- update `dlv` packed with plugins.

## 3.16.2

### Fixed

- Add go to skipped processes in JetBrains plugin. Solving GoLand bug.

## 3.16.1

### Fixed

- Running on specific Kubernetes setups, such as Docker for Desktop should work again.

## 3.16.0

### Added

- Add golang stat hooks, closes [#856](https://github.com/metalbear-co/mirrord/issues/856)

### Fixed

- agent: mount /var from host and reconfigure docker socket to /var/run/docker.sock for better compatibility
- Error on specifying namespace in configuration without path (pod/container/deployment). Closes [#830](https://github.com/metalbear-co/mirrord/issues/830)
- IntelliJ plugin with new UI enabled now shows buttons. Closes [#881](https://github.com/metalbear-co/mirrord/issues/881)
- Fix deprecation warnings (partially), update checkout action to version 3.

### Changed

- Refactored detours to use new helper function `Result::as_hook` to simplify flow. (no change in behavior)

## 3.15.2

### Added

- Logging for IntelliJ plugin for debugging/bug reports.

### Fixed

- Crash when mirroring and state is different between local and remote (happens in Mesh).
  We now ignore messages that are not in the expected state. (as we can't do anything about it).
- agent: Fix typo in socket path for k3s environments
- intellij-plugin: fix missing telemetry/version check

## 3.15.1

### Added

- Add `__xstat` hook, fixes [#867]((https://github.com/metalbear-co/mirrord/issues/867))

### Fixed

- Fix build scripts for the refactored IntelliJ plugin

## 3.15.0

### Added

- agent: Add support for k3s envs
- IntelliJ plugin - refactor, uses cli like vs code.

### Fixed

- getaddrinfo: if node is NULL just bypass, as it's just for choosing ip/port, Fixes[#858](https://github.com/metalbear-co/mirrord/issues/858) and [#848](https://github.com/metalbear-co/mirrord/issues/848)

### Changed

- cli now loads env, removes go env stuff at load, might fix some bugs there.

## 3.14.3

### Fixed

- Create empty release to overcome temporary issue with VS Code marketplace publication

## 3.14.2

### Fixed

- vscode ext: use process env for running mirrord. Fixes [#854](https://github.com/metalbear-co/mirrord/issues/854)

## 3.14.1

### Fixed

- layer + go - connect didn't intercept sometimes (we lacked a match). Fixes [851](https://github.com/metalbear-co/mirrord/issues/851).

## 3.14.0

### Changed

- cli: Set environment variables from cli to spawned process instead of layer when using `mirrord exec`.
- cli: use miette for nicer errors
- cli: some ext exec preparations, nothing user facing yet.
- vs code ext: use cli, fixes some env bugs with go and better user experience.

## 3.13.5

### Changed

- Don't add temp prefix when using `extract` command.
- VS Code extension: mirrord enable/disable to be per workspace.
- VS Code extension: bundle the resources
- Add `/System` to default ignore list.
- Remove `test_mirrord_layer` from CI as it's covered in integration testing.

### Fixed

- fd leak on Linux when using libuv (Node). Caused undefined behavior. Fixes [#757](https://github.com/metalbear-co/mirrord/issues/757).

### Misc

- Better separation in mirrord cli.

## 3.13.4

### Changed

- Adjust filters - all directory filters also filter the directory itself (for when lstat/stating the directory).
  Added `/Applications`

## 3.13.3

### Added

- Add `mirrord ls` which allows listing target path. Hidden from user at the moment, as for now it's meant for extension use only.

### Changed

- Refactor e2e tests: split into modules based on functionality they test.
- internal refactor in mirrord-agent: Stealer feature changed from working per connection to now starting with
  the agent itself ("global"). Got rid of `steal_worker` in favor of a similar abstraction to what
  we have in `sniffer.rs` (`TcpConnectionStealer` that acts as the traffic stealing task, and
  `TcpStealerApi` which bridges the communication between the agent and the stealer task).
- Tests CI: don't wait for integration tests to start testing E2E tests.

### Fixed

- Add missing `fstat`/`lstat`/`fstatat`/`stat` hooks.

## 3.13.2

### Fixed

- Weird crash that started happening after Frida upgrade on macOS M1.

## 3.13.1

### Fixed

- Fix asdf:
  - Add `/tmp` not just `/tmp/` to exclusion.
  - Add `.tool-version` to exclusion.
  - `fclose` was calling close which doesn't flush.

## 3.13.0

### Changed

- IntelliJ Plugin: downgrade Java to version 11.
- IntelliJ Plugin: update platform version to 2022.3.
- Disable progress in mirrord-layer - can cause issues with forks and generally confusing now
  that agent is created by cli (and soon to be created by IDE plugin via cli).
- Update to Frida 16.0.7
- Add more paths to the default ignore list (`/snap` and `*/.asdf/*`) - to fix asdf issues.
- Add `/bin/` to default ignore list - asdf should be okay now!
- Update GitHub action to use latest `rust-cache`

### Added

- mirrord-operator: Add securityContext section for deployment in operator setup

### Fixed

- Fix `--fs-mode=local` didn't disable hooks as it was supposed to.
- Fix hooking wrong libc functions because of lack of module specification - add function to resolve
  module name to hook from (libc on Unix,libsystem on macOS). Partially fixes asdf issue.

## 3.12.1

### Added

- E2E test for pause feature with service that logs http requests and a service that makes requests.
- mirrord-layer: automatic operator discovery and connection if deployed on cluster. (Discovery can be disabled with `MIRRORD_OPERATOR_ENABLE=false`).

### Changed

- Added `/tmp/` to be excluded from file ops by default. Fixes [#800](https://github.com/metalbear-co/mirrord/issues/800).

### Misc

- Reformatted a bit the file stuff, to make it more readable. We now have `FILE_MODE` instead of `FILE_OPS_*` internally.
- Changed fileops test to also test write override (mirrord mode is read and override specific path)

## 3.12.0

### Added

- `--pause` feature (unstable). See [#712](https://github.com/metalbear-co/mirrord/issues/712).
- operator setup cli feature.
- mirrord-layer: operator connection that can be used instad of using kubernetes api to access agents.

### Changed

- CI: cancel previous runs of same PR.
- cli: set canonical path for config file to avoid possible issues when child processes change current working directory.
- config: Refactor config proc macro and behavior - we now error if a config value is wrong instead of defaulting.
- layer: panic on error instead of exiting without any message.
- CI: don't run CI on draft PRs.
- Update dependencies.
- Update to clap v4 (cli parser crate).
- Started deprecation of fsmode=disabled, use fsmode=local instead.

### Fixed

- Typo in `--agent-startup-timeout` flag.

## 3.11.2

### Fixed

- Agent dockerfile: fix build for cross arch

### Changed

- Added clippy on macOS and cleaned warnings.

## 3.11.1

### Fixed

- release.yaml: Linux AArch64 for real this time. (embedded so was x64)

### Changed

- Create agent in the cli and pass environment variables to exec'd process to improve agent re-use.
- IntelliJ: change default log level to warning (match cli/vscode).
- IntelliJ: don't show progress (can make some tests/scenarios fail).
- release.yaml: Build layer/cli with Centos 7 compatible glibc (AmazonLinux2 support).
- Change CPU/memory values requested by the Job agent to the lowest values possible.

## 3.11.0

### Added

- MacOS: Support for executing SIP binaries in user applications. We hook `execve`
  and create a SIP-free version of the binary on-the-go and execute that instead of
  the SIP binary.
  This means we now support running bash scripts with mirrord also on MacOS.
  Closes [#649](https://github.com/metalbear-co/mirrord/issues/649).

### Changed

- Only warn about invalid certificates once per agent.
- Reduce tokio features to needed ones only.

### Fixed

- CI: Fix regex for homebrew formula
- Potentially ignoring write calls (`fd < 2`).
- CI: Fix release for linux aarch64. Fixes [#760](https://github.com/metalbear-co/mirrord/issues/760).
- Possible cases where we don't close fds correctly.

## 3.10.4

### Fixed

- VS Code Extension: Fix crash when no env vars are defined in launch.json

## 3.10.3

### Changed

- CLI: change temp lib file to only be created for new versions
- mirrord-config: refactored macro so future implementations will be easier

### Fixed

- Release: fix homebrew release step

## 3.10.2

### Fixed

- CI: fix `release_gh` zip file step

## 3.10.1

### Changed

- CI: download shasums and add git username/email to make the homebrew release work
- Remove `unimplemented` for some IO cases, we now return `Unknown` instead. Also added warning logs for these cases to track.
- Only recommend `--accept-invalid-certificates` on connection errors if not already set.
- Terminate user application on connection error instead of only stopping mirrord.

## 3.10.0

### Added

- CI: Update homebrew formula on release, refer [#484](https://github.com/metalbear-co/mirrord/issues/484)

### Changed

- VS Code Extension: change extension to use the target specified in the mirrord config file, if specified, rather than show the pod dropdown

## 3.9.0

### Added

- `MIRRORD_AGENT_NETWORK_INTERFACE` environment variable/file config to let user control which network interface to use. Workaround for [#670](https://github.com/metalbear-co/mirrord/issues/670).
- mirrord-config: `deprecated` and `unstable` tags to MirrordConfg macro for messaging user when using said fields

### Changed

- VS Code Extension: change extension to use a mirrord-config file for configuration
- VS Code Extension: use the IDE's telemetry settings to determine if telemetry should be enabled

## 3.8.0

### Changed

- mirrord-layer: Remove `unwrap` from initialization functions.
- Log level of operation bypassing log from warn to trace (for real this time).
- Perform filesystem operations for paths in `/home` locally by default (for real this time).

### Added

- VS Code Extension: add JSON schema
- Bypass SIP on MacOS on the executed binary, (also via shebang).
  See [[#649](https://github.com/metalbear-co/mirrord/issues/649)].
  This does not yet include binaries that are executed by the first binary.

### Fixed

- fix markdown job by adding the checkout action

## 3.7.3

### Fixed

- mirrord-agent: No longer resolves to `eth0` by default, now we first try to resolve
  the appropriate network interface, if this fails then we use `eth0` as a last resort.
  Fixes [#670](https://github.com/metalbear-co/mirrord/issues/670).

### Changed

- intelliJ: use custom delve on macos

## 3.7.2

### Fixed

- Release: fix broken docker build step caused by folder restructure

## 3.7.1

### Fixed

- using gcloud auth for kubernetes. (mistakenly loaded layer into it)
- debugging Go on VSCode. We patch to use our own delivered delve.
- Changed layer not to crash when connection is closed by agent. Closed [#693](https://github.com/metalbear-co/mirrord/issues/693).

### Changed

- IntelliJ: fallback to using a textfield if listing namespaces fails

## 3.7.0

### Added

- mirrord-config: New `mirrord-schema.json` file that contains docs and types which should help the user write their mirrord
  config files. This file has to be manually generated (there is a test to help you remember).

### Fixed

- IntelliJ: Fix occurring of small namespace selection window and make mirrord dialogs resizable
- IntelliJ: Fix bug when pressing cancel in mirrord dialog and rerunning the application no mirrord window appears again
- VS Code: Fix crash occurring because it used deprecated env vars.

### Changed

- mirrord-config: Take `schema` feature out of feature flag (now it's always on).
- mirrord-config: Add docs for the user config types.

## 3.6.0

### Added

- mirrord-layer: Allow capturing tracing logs to file and print github issue creation link via MIRRORD_CAPTURE_ERROR_TRACE env variable

### Fixed

- Fix vscode artifacts where arm64 package was not released.
- IntelliJ plugin: if namespaces can't be accessed, use the default namespace

### Changed

- Add `/home` to default file exclude list.
- Changed log level of `Bypassing operation...` from warning to trace.
- IntelliJ settings default to match CLI/VSCode.

## 3.5.3

### Fixed

- Fixed broken release step for VS Code Darwin arm64 version

## 3.5.2

### Fixed

- Fixed breaking vscode release step

## 3.5.1

### Fixed

- Fixed an issue with the release CI

### Changed

- Update target file config to have `namespace` nested inside of `target` and not a separate `target_namespace`.
  See [#587](https://github.com/metalbear-co/mirrord/issues/587) and [#667](https://github.com/metalbear-co/mirrord/issues/667)

## 3.5.0

### Added

- aarch64 release binaries (no go support yet, no IntelliJ also).
- mirrord-layer: Add [`FileFilter`](mirrord-layer/src/file/filter.rs) that allows the user to include or exclude file paths (with regex support) for file operations.

### Changed

- mirrord-layer: Improve error message when user tries to run a program with args without `--`.
- Add tests for environment variables passed to KubeApi for authentication feature for cli credential fetch
- Remove openssl/libssl dependency, cross compilation is easier now. (It wasn't needed/used)
- mirrord-config: Changed the way [`fs`](mirrord-config/src/fs.rs) works: now it supports 2 modes `Simple` and `Advanced`,
  where `Simple` is similar to the old behavior (enables read-only, read-write, or disable file ops), and `Advanced`
  allows the user to specify include and exclude (regexes) filters for [`FileFilter`](mirrord-layer/src/file/filter.rs).
- Lint `README` and update it for `--target` flag.
- mirrord-layer: improve error message for invalid targets.

### Removed

- `--pod-name`, `--pod-namespace`, `--impersonated_container_name` have been removed in favor of `--target`, `--target-namespace`

### Fixed

- Env var to ignore ports used by a debugger for intelliJ/VSCode, refer [#644](https://github.com/metalbear-co/mirrord/issues/644)

## 3.4.0

### Added

- Add changelog for intelliJ extension, closes [#542](https://github.com/metalbear-co/mirrord/issues/542)
- Add filter for changelog to ci.yml
- Telemetry for intelliJ extension.

### Changed

- Update intelliJ extension: lint & bump java version to 17.
- Added `/Users` and `/Library` to path to ignore for file operations to improve UX on macOS.
- Use same default options as CLI in intelliJ extension.
- Improve UI layout of intelliJ extension.
- Separate tcp and udp outgoing option in intelliJ extension.
- Tighter control of witch environment variables would be passed to the KubeApi when fetching credentials via cli in kube-config. See [#637](https://github.com/metalbear-co/mirrord/issues/637)

### Fixed

- Lint Changelog and fix level of a "Changed" tag.
- File operations - following symlinks now works as expected. Previously, absolute symlinks lead to use our own path instead of target path.
  For example, AWS/K8S uses `/var/run/..` for service account credentials. In many machines, `/var/run` is symlink to `/run`
  so we were using `/run/..` instead of `/proc/{target_pid}/root/run`.
- Fix not reappearing window after pressing cancel-button in intelliJ extension.

## 3.3.0

### Added

- Telemetries, see [TELEMETRY.md](./TELEMETRY.md) for more information.

### Changed

- Added timeout for "waiting for pod to be ready..." in mirrord-layer to prevent unresponsive behavior. See [#579](https://github.com/metalbear-co/mirrord/issues/579)
- IntelliJ Extension: Default log level to `ERROR` from `DEBUG`

### Fixed

- Issue with [bottlerocket](https://github.com/bottlerocket-os/bottlerocket) where they use `/run/dockershim.sock`
  instead of the default containerd path. Add new path as fallback.

## 3.2.0

### Changed

- Extended support for both `-s` and `-x` wildcard matching, now supports `PREFIX_*`, `*_SUFFIX`, ect.
- Add to env default ignore `JAVA_HOME`,`HOMEPATH`,`CLASSPATH`,`JAVA_EXE` as it's usually runtime that you don't want
  from remote. Possibly fixes issue discussed on Discord (used complained that they had to use absolute path and not
  relative).
- Add `jvm.cfg` to default bypass for files.
- Clarify wrong target error message.
- mirrord-layer: Improve error message in `connection::handle_error`.

### Fixed

- Don't ignore passed `--pod-namespace` argument, closes
  [[#605](https://github.com/metalbear-co/mirrord/issues/605)]
- Replace deprecated environment variables in IntelliJ plugin
- Issues with IntelliJ extension when debugging Kotlin applications
- Scrollable list for pods and namespaces for IntelliJ extension,
  closes [[#610](https://github.com/metalbear-co/mirrord/issues/610)]

### Deprecated

- `--impersonated-container-name` and `MIRRORD_IMPERSONATED_CONTAINER_NAME` are
  deprecated in favor of `--target` or `MIRRORD_IMPERSONATED_TARGET`
- `--pod-namespace` and `MIRRORD_AGENT_IMPERSONATED_POD_NAMESPACE` are deprecated in
  favor of `--target-namespace` and `MIRRORD_TARGET_NAMESPACE`

## 3.1.3

### Changed

- release: VS Code extension release as stable and not pre-release.

### Fixed

- Dev container failing to execute `apt-get install -y clang`

## 3.1.2

### Changed

- Update some texts in documentation, READMEs, and extension package descriptions
- IntelliJ version check on enabling instead of on project start. Don't check again after less than 3 minutes.

## 3.1.1

### Fixed

- IntelliJ plugin crashing on run because both include and exclude were being set for env vars.

## 3.1.0

### Added

- `pwrite` hook (used by `dotnet`);

### Fixed

- Issue [#577](https://github.com/metalbear-co/mirrord/issues/577). Changed non-error logs from `error!` to `trace!`.

### Changed

- Agent pod definition now has `requests` specifications to avoid being defaulted to high values.
  See [#579](https://github.com/metalbear-co/mirrord/issues/579).
- Change VSCode extension configuration to have file ops, outgoing traffic, DNS, and environment variables turned on by
  default.
- update intelliJ extension: toggles + panel for include/exclude env vars

## 3.0.22-alpha

### Changed

- Exclude internal configuration fields from generated schema.

### Fixed

- Issue [#531](https://github.com/metalbear-co/mirrord/issues/531). We now detect NixOS/Devbox usage and add `sh` to
  skipped list.

## 3.0.21-alpha

### Added

- Reuse agent - first process that runs will create the agent and its children will be able to reuse the same one to
  avoid creating many agents.
- Don't print progress for child processes to avoid confusion.
- Skip istio/linkerd-proxy/init container when mirroring a pod without a specific container name.
- Add "linkerd.io/inject": "disabled" annotation to pod created by mirrord to avoid linkerd auto inject.
- mirrord-layer: support `-target deployment/deployment_name/container/container_name` flag to run on a specific
  container.
- `/nix/*` path is now ignored for file operations to support NixOS.
- Shortcut `deploy` for `deployment` in target argument.
- Added the ability to override environment variables in the config file.

### Changed

- Print exit message when terminating application due to an unhandled error in the layer.
- mirrord-layer: refactored `pod_api.rs` to be more maintainble.
- Use kube config namespace by default.
- mirrord-layer: Ignore `EAFNOSUPPORT` error reporting (valid scenario).

## 3.0.20-alpha

### Added

- `pread` hook (used by `dotnet`);
- mirrord-layer: ignore opening self-binary (temporal SDK calculates the hash of the binary, and it fails because it
  happens remotely)
- Layer integration tests with more apps (testing with Go only on MacOS because of
  known crash on Linux - [[#380](https://github.com/metalbear-co/mirrord/issues/380)]).
  Closes [[#472](https://github.com/metalbear-co/mirrord/issues/472)].
- Added progress reporting to the CLI.
- CI: use [bors](https://bors.tech/) for merging! woohoo.

### Changed

- Don't report InProgress io error as error (log as info)
- mirrord-layer: Added some `dotnet` files to `IGNORE_FILES` regex set;
- mirrord-layer: Added the `Detour` type for use in the `ops` modules instead of `HookResult`. This type supports
  returning a `Bypass` to avoid manually checking if a hook actually failed or if we should just bypass it;
- mirrord-protocol: Reduce duplicated types around `read` operation;
- Layer integration tests for more apps. Closes
  [[#472](https://github.com/metalbear-co/mirrord/issues/472)].
- Rename http mirroring tests from `integration` to `http_mirroring` since there are
  now also integration tests in other files.
- Delete useless `e2e_macos` CI job.
- Integration tests also display test process output (with mirrord logs) when they
  time out.
- CI: mirrord-layer UT and integration run in same job.
- .devcontainer: Added missing dependencies and also kind for running e2e tests.

### Fixed

- Fix IntelliJ Extension artifact - use glob pattern
- Use LabelSelector instead of app=* to select pods from deployments
- Added another
  protection [to not execute in child processes from k8s auth](https://github.com/metalbear-co/mirrord/issues/531) by
  setting an env flag to avoid loading then removing it after executing the api.

## 3.0.19-alpha

### Added

- Release image for armv7 (Cloud ARM)

### Fixed

- Release for non-amd64 arch failed because of lack of QEMU step in the github action. Re-added it

## 3.0.18-alpha

### Changed

- Replaced `pcap` dependency with our own `rawsocket` to make cross compiling faster and easier.

## 3.0.17-alpha

### Fixed

- Release CI: Remove another failing step

## 3.0.16-alpha

### Fixed

- Release CI: Temporarily comment out failing step

## 3.0.15-alpha

### Fixed

- Release CI: Fix checkout action position in intelliJ release.

## 3.0.14-alpha

### Added

- Layer integration test. Tests the layer's loading and hooking in an http mirroring simulation with a flask web app.
  Addresses but does not
  close [[#472](https://github.com/metalbear-co/mirrord/issues/472)] (more integration tests still needed).

### Fixed

- Release CI: Fix paths for release artifacts

## 3.0.13-alpha

### Added

- mirrord-cli: added a SIP protection check for macos binaries,
  closes [[#412](https://github.com/metalbear-co/mirrord/issues/412)]

### Fixed

- Fixed unused dependencies issue, closes [[#494](https://github.com/metalbear-co/mirrord/issues/494)]

### Changed

- Remove building of arm64 Docker image from the release CI

## 3.0.12-alpha

### Added

- Release CI: add extensions as artifacts, closes [[#355](https://github.com/metalbear-co/mirrord/issues/355)]

### Changed

- Remote operations that fail logged on `info` level instead of `error` because having a file not found, connection
  failed, etc can be part of a valid successful flow.
- mirrord-layer: When handling an outgoing connection to localhost, check first if it's a socket we intercept/mirror,
  then just let it connect normally.
- mirrord-layer: removed `tracing::instrument` from `*_detour` functions.

### Fixed

- `getaddrinfo` now uses [`trust-dns-resolver`](https://docs.rs/trust-dns-resolver/latest/trust_dns_resolver/) when
  resolving DNS (previously it would do a `getaddrinfo` call in mirrord-agent that could result in incompatibility
  between the mirrored pod and the user environments).
- Support clusters running Istio. Closes [[#485](https://github.com/metalbear-co/mirrord/issues/485)].

## 3.0.11-alpha

### Added

- Support impersonated deployments, closes [[#293](https://github.com/metalbear-co/mirrord/issues/293)]
- Shorter way to select which deployment/pod/container to impersonate through `--target`
  or `MIRRORD_IMPERSONATED_TARGET`, closes [[#392](https://github.com/metalbear-co/mirrord/issues/392)]
- mirrord-layer: Support config from file alongside environment variables.
- intellij-ext: Add version check, closes [[#289](https://github.com/metalbear-co/mirrord/issues/289)]
- intellij-ext: better support for Windows with WSL.

### Deprecated

- `--pod-name` or `MIRRORD_AGENT_IMPERSONATED_POD_NAME` is deprecated in favor of `--target`
  or `MIRRORD_IMPERSONATED_TARGET`

### Fixed

- tcp-steal working with linkerd meshing.
- mirrord-layer should exit when agent disconnects or unable to make initial connection

## 3.0.10-alpha

### Added

- Test that verifies that outgoing UDP traffic (only with a bind to non-0 port and a
  call to `connect`) is successfully intercepted and forwarded.

### Fixed

- macOS binaries should be okay now.

## 3.0.9-alpha

### Changed

- Ignore http tests because they are unstable, and they block the CI.
- Bundle arm64 binary into the universal binary for MacOS.

## 3.0.8-alpha

### Fixed

- release CI: Fix dylib path for `dd`.

## 3.0.7-alpha

### Fixed

- mirrord-layer: Fix `connect` returning error when called on UDP sockets and the
  outgoing traffic feature of mirrord is disabled.
- mirrord-agent: Add a `tokio::time:timeout` to `TcpStream::connect`, fixes golang issue where sometimes it would get
  stuck attempting to connect on IPv6.
- intelliJ-ext: Fix CLion crash issue, closes [[#317](https://github.com/metalbear-co/mirrord/issues/317)]
- vscode-ext: Support debugging Go, and fix issues with configuring file ops and traffic stealing.

### Changed

- mirrord-layer: Remove check for ignored IP (localhost) from `connect`.
- mirrord-layer: Refactor `connect` function to be less bloated.
- `.dockerignore` now ignores more useless files (reduces mirrord-agent image build time, and size).
- mirrord-agent: Use `tracing::instrument` for the outgoing traffic feature.
- mirrord-agent: `IndexAllocator` now uses `ConnectionId` for outgoing traffic feature.

## 3.0.6-alpha

### Changed

- mirrord-layer: Remove `tracing::instrument` from `go_env::goenvs_unix_detour`.
- mirrord-layer: Log to info instead of error when failing to write to local tunneled streams.

### Added

- mirrord-layer, mirrord-cli: new command line argument/environment variable - `MIRRORD_SKIP_PROCESSES` to provide a
  list of comma separated processes to not to load into.
  Closes [[#298](https://github.com/metalbear-co/mirrord/issues/298)]
  , [[#308](https://github.com/metalbear-co/mirrord/issues/308)]
- release CI: add arm64e to the universal dylib
- intellij-ext: Add support for Goland

## 3.0.5-alpha

### Fixed

- mirrord-layer: Return errors from agent when `connect` fails back to the hook (previously we were handling these as
  errors in layer, so `connect` had slightly wrong behavior).
- mirrord-layer: instrumenting error when `write_detur` is called to stdout/stderr
- mirrord-layer: workaround for `presented server name type wasn't supported` error when Kubernetes server has IP for CN
  in certificate. [[#388](https://github.com/metalbear-co/mirrord/issues/388)]

### Changed

- mirrord-layer: Use `tracing::instrument` to improve logs.

### Added

- Outgoing UDP test with node. Closes [[#323](https://github.com/metalbear-co/mirrord/issues/323)]

## 3.0.4-alpha

### Fixed

- Fix crash in VS Code extension happening because the MIRRORD_OVERRIDE_ENV_VARS_INCLUDE and
  MIRRORD_OVERRIDE_ENV_VARS_EXCLUDE vars being populated with empty values (rather than not being populated at all)
  .Closes [[#413](https://github.com/metalbear-co/mirrord/issues/413)].
- Add exception to gradle when dylib/so file is not found.
  Closes [[#345](https://github.com/metalbear-co/mirrord/issues/345)]
- mirrord-layer: Return errors from agent when `connect` fails back to the hook (previously we were handling these as
  errors in layer, so `connect` had slightly wrong behavior).

## 3.0.3-alpha

### Changed

- Changed agent namespace to default to the pod namespace.
  Closes [[#404](https://github.com/metalbear-co/mirrord/issues/404)].

## 3.0.2-alpha

### Added

- Code sign Apple binaries.
- CD - Update latest tag after release is published.

### Changed

- In `go-e2e` test, call `os.Exit` instead fo sending `SIGINT` to the process.
- Install script now downloads latest tag instead of main branch to avoid downtime on installs.

### Fixed

- Fix Environment parsing error when value contained '='
  Closes [[#387](https://github.com/metalbear-co/mirrord/issues/387)].
- Fix bug in outgoing traffic with multiple requests in quick succession.
  Closes [[#331](https://github.com/metalbear-co/mirrord/issues/331)].

## 3.0.1-alpha

### Fixed

- Add missing dependency breaking the VS Code release.

## 3.0.0-alpha

### Added

- New feature: UDP outgoing, mainly for Go DNS but should work for most use cases also!
- E2E: add tests for python's fastapi with uvicorn
- Socket ops - `connect`: ignore localhost and ports 50000 - 60000 (reserved for debugger)
- Add "*.plist" to `IGNORE_REGEX`, refer [[#350](https://github.com/metalbear-co/mirrord/issues/350)].

### Changed

- Change all functionality (incoming traffic mirroring, remote DNS outgoing traffic, environment variables, file reads)
  to be enabled by default. ***Note that flags now disable functionality***

### Fixed

- mirrord-layer: User-friendly error for invalid kubernetes api certificate
- mirrord-cli: Add random prefix to the generated shared lib to prevent Bus Error/EXC_BAD_ACCESS
- Support for Go 1.19>= syscall hooking
- Fix Python debugger crash in VS Code Extension. Closes [[#350](https://github.com/metalbear-co/mirrord/issues/350)].

## 2.13.0

### Added

- Release arm64 agent image.

### Fixed

- Use selected namespace in IntelliJ plugin instead of always using default namespace.

## 2.12.1

### Fixed

- Fix bug where VS Code extension would crash on startup due to new configuration values not being the correct type.
- Unset DYLD_INSERT_LIBRARIES/LD_PRELOAD when creating the agent.
  Closes [[#330](https://github.com/metalbear-co/mirrord/issues/330)].
- Fix NullPointerException in IntelliJ Extension. Closes [[#335](https://github.com/metalbear-co/mirrord/issues/335)].
- FIx dylib/so paths for the IntelliJ Extension. Closes [[#337](https://github.com/metalbear-co/mirrord/pull/352)].

## 2.12.0

### Added

- Add more configuration values to the VS Code extension.
- Warning when using remote tcp without remote DNS (can cause ipv6/v4 issues).
  Closes [#327](https://github.com/metalbear-co/mirrord/issues/327)

### Fixed

- VS Code needed restart to apply kubectl config/context change.
  Closes [316](https://github.com/metalbear-co/mirrord/issues/316).
- Fixed DNS feature causing crash on macOS on invalid DNS name due to mismatch of return
  codes. [#321](https://github.com/metalbear-co/mirrord/issues/321).
- Fixed DNS feature not using impersonated container namespace, resulting with incorrect resolved DNS names.
- mirrord-agent: Use `IndexAllocator` to properly generate `ConnectionId`s for the tcp outgoing feature.
- tests: Fix outgoing and DNS tests that were passing invalid flags to mirrord.
- Go Hooks - use global ENABLED_FILE_OPS
- Support macOS with apple chip in the IntelliJ plugin.
  Closes [#337](https://github.com/metalbear-co/mirrord/issues/337).

## 2.11.0

### Added

- New feature: mirrord now supports TCP traffic stealing instead of mirroring. You can enable it by
  passing `--tcp-steal` flag to cli.

### Fixed

- mirrord-layer: Go environment variables crash - run Go env setup in a different stack (should
  fix [#292](https://github.com/metalbear-co/mirrord/issues/292))

### Changed

- mirrord-layer: Add `#![feature(let_chains)]` to `lib.rs` to support new compiler version.

## 2.10.1

### Fixed

- CI:Release - Fix typo that broke the build

## 2.10.0

### Added

- New feature, [tcp outgoing traffic](https://github.com/metalbear-co/mirrord/issues/27). It's now possible to make
  requests to a remote host from the staging environment context. You can enable this feature setting
  the `MIRRORD_TCP_OUTGOING` variable to true, or using the `-o` option in mirrord-cli.
- mirrord-cli add login command for logging in to metalbear-cloud
- CI:Release - Provide zip and sha256 sums

### Fixed

- Environment variables feature on Golang programs. Issue #292 closed in #299

## 2.9.1

### Fixed

- CI - set typescript version at 4.7.4 to fix broken release action

## 2.9.0

### Added

- Support for Golang fileops
- IntelliJ Extension for mirrord

### Changed

- mirrord-layer: Added common `Result` type to to reduce boilerplate, removed dependency of `anyhow` crate.
- mirrord-layer: Split `LayerError` into `LayerError` and `HookError` to distinguish between errors that can be handled
  by the layer and errors that can be handled by the hook. (no more requiring libc errno for each error!).
  Closes [#247](https://github.com/metalbear-co/mirrord/issues/247)

## 2.8.1

### Fixed

- CI - remove usage of ubuntu-18.04 machines (deprecated)

## 2.8.0

### Added

- E2E - add basic env tests for bash scripts

### Fixed

- mirrord-agent - Update pcap library, hopefully will fix dropped packets (syn sometimes missed in e2e).
- mirrord-agent/layer - Sometimes layer tries to connect to agent before it finsihed loading, even though pod is
  running. Added watching the log stream for a "ready" log message before attempting to connect.

### Changed

- E2E - describe all pods on failure and add file name to print of logs.
- E2E - print timestamp of stdout/stderr of `TestProcess`.
- E2E - Don't delete pod/service on failure, instead leave them for debugging.
- mirrord-agent - Don't use `tokio::spawn` for spawning `sniffer` (or any other namespace changing task) to avoid
  namespace-clashing/undefined behavior. Possibly fixing bugs.
- Change the version check on the VS Code extension to happen when mirrord is enabled rather than when the IDE starts
  up.

## 2.7.0

### Added

- mirrord-layer: You can now pass `MIRRORD_AGENT_COMMUNICATION_TIMEOUT` as environment variable to control agent
  timeout.
- Expand file system operations with `access` and `faccessat` hooks for absolute paths

### Fixed

- Ephemeral Containers didn't wait for the right condition, leading to timeouts in many cases.
- mirrord-layer: Wait for the correct condition in job creation, resolving startup/timeout issues.
- mirrord-layer: Add a sleep on closing local socket after receiving close to let local application respond before
  closing.
- mirrord-layer: Fix DNS issue where `ai_addr` would not live long enough (breaking the remote DNS feature).

### Changed

- Removed unused dependencies from `mirrord-layer/Cargo.toml`. (Closes #220)
- reduce e2e flakiness (add message sent on tcp listen subscription, wait for that message)
- reduce e2e flakiness - increase timeout time
- mirrord-layer - increase agent creation timeout (to reduce e2e flakiness on macOS)
- E2E - Don't do file stuff on http traffic to reduce flakiness (doesn't add any coverage value..)
- mirrord-layer - Change tcp mirror tunnel `select` to be biased so it flushes all data before closing it (better
  testing, reduces e2e flakiness)
- E2E - unify resolve_node_host for linux and macOS with support for wsl provided Docker & Kubernetes
- E2E - add `trace` for tests to have paramaterized arguments printed
- mirrord-agent - add debug print of args to identify runs
- E2E - remove double `--extract-path` parameter in tests
- E2E - macOS colima start with 3 cores and 8GB of RAM.
- E2E - Increase agent communication timeout to reduce flakiness.
- mirrord-layer - add `DetourGuard` to prevent unwanted calls to detours from our code.
- mirrord-layer - extract reused detours to seperate logic functions
- E2E - macOS run only sanity http mirror traffic with Python

## 2.6.0

### Added

- Add a flag for the agent, `--ephemeral-container`, to correctly refer to the filesystem i.e. refer to root path
  as `/proc/1/root` when the flag is on, otherwise `/`.
- Add support for Golang on amd64 (x86-64).

### Changed

- Assign a random port number instead of `61337`. (Reason: A forking process creates multiple agents sending traffic on
  the same port, causing addrinuse error.)
- `mirrord-layer/socket` now uses `socket2::SockAddr` to comply with Rust's new IP format.

### Fixed

- Fix filesystem tests to only run if the default path exists.
- Fix extension not running due to the node_modules directory not being packaged.

## 2.5.0

### Added

- New feature, [remote DNS resolving](https://github.com/metalbear-co/mirrord/issues/27#issuecomment-1154072686).
  It is now possible to use the remote's `addrinfo` by setting the `MIRRORD_REMOTE_DNS` variable to
  `true`, or using the `-d` option in mirrord-cli.
- New feature, [Ephemeral Containers](https://github.com/metalbear-co/mirrord/issues/172).
  Use Kubernetes beta feature `Ephemeral Containers` to mirror traffic with the `--ephemeral-container` flag.
- E2E tests on macos for Golang using the Gin framework.

### Changed

- Refactored `mirrord-layer/socket` into a module structure similar to `mirrord-layer/file`.
- Refactored the error part of the many `Result<Response, ResponseError>`.
- Refactored `file` related functions, created `FileHandler` and improved structure.
- Refactored error handling in mirrord-layer.
- E2E: Collect minikube logs and fix collecting container logs
- E2E: macOS use colima instead of minikube.
- Refactored `mirrord-layer/lib.rs` - no more passing many arguments! :)
- Refactored `mirrord-layer/lib.rs` - remove `unwrap()` and propagate error using `Result`

### Fixed

- Handle unwraps in fileops to gracefully exit and enable python fileops tests.
- Changed `addrinfo` to `VecDeque` - fixes a potential bug (loss of order)

## 2.4.1

### Added

- mirrord-cli `exec` subcommand accepts `--extract-path` argument to set the directory to extract the library to. Used
  for tests mainly.
- mirrord-layer provides `MIRRORD_IMPERSONATED_CONTAINER_NAME` environment variable to specify container name to
  impersonate. mirrord-cli accepts argument to set variable.
- vscode-ext provides quick-select for setting `MIRRORD_IMPERSONATED_CONTAINER_NAME`

### Changed

- Refactor e2e, enable only Node HTTP mirroring test.
- E2E: add macOS to E2E, support using minikube by env var.
- E2E: Skip loading to docker before loading to minikube (load directly to minikube..)
- layer: Environment variables now load before process starts, no more race conditions.

### Fixed

- Support connections that start with tcp flags in addition to Syn (on macOS CI we saw CWR + NS)
- `fcntl` error on macOS [#184](https://github.com/metalbear-co/mirrord/issues/184) by a workaround.

## 2.3.1

### Changed

- Refactor(agent) - change `FileManager` to be per peer, thus removing the need of it being in a different task, moving
  the handling to the peer logic, change structure of peer handling to a struct.
- Don't fail environment variable request if none exists.
- E2E: Don't assert jobs and pods length, to allow better debugging and less flakiness.
- Refactor(agent) - Main loop doesn't pass messages around but instead spawned peers interact directly with tcp sniffer.
  Renamed Peer -> Client and ClientID.
- Add context to agent/job creation errors (Fixes #112)
- Add context to stream creation error (Fixes #110)
- Change E2E to use real app, closes [#149](https://github.com/metalbear-co/mirrord/issues/149)

## 2.3.0

### Added

- Add support for overriding a process' environment variables by setting `MIRRORD_OVERRIDE_ENV_VARS` to `true`. To
  filter out undesired variables, use the `MIRRORD_OVERRIDE_FILTER_ENV_VARS` configuration with arguments such
  as `FOO;BAR`.

### Changed

- Remove `unwrap` from the `Future` that was waiting for Kube pod to spin up in `pod_api.rs`. (Fixes #110)
- Speed up agent container image building by using a more specific base image.
- CI: Remove building agent before building & running tests (duplicate)
- CI: Add Docker cache to Docker build-push action to reduce build duration.
- CD release: Fix universal binary for macOS
- Refactor: Change protocol + mirrord-layer to split messages into modules, so main module only handles general
  messages, passing down to the appropriate module for handling.
- Add a CLI flag to specify `MIRRORD_AGENT_TTL`
- CI: Collect mirrord-agent logs in case of failure in e2e.
- Add "app" = "mirrord" label to the agent pod for log collection at ease.
- CI: Add sleep after local app finishes loading for agent to load filter make tests less flaky.
- Handle relative paths for open, openat
- Fix once cell renamings, PR [#98165](https://github.com/rust-lang/rust/pull/98165)
- Enable the blocking feature of the `reqwest` library

## 2.2.1

### Changed

- Compile universal binaries for MacOS. (Fixes #131)
- E2E small improvements, removing sleeps. (Fixes #99)

## 2.2.0

### Added

- File operations are now available behind the `MIRRORD_FILE_OPS` env variable, this means that mirrord now hooks into
  the following file functions: `open`, `fopen`, `fdopen`, `openat`, `read`, `fread`, `fileno`, `lseek`, and `write` to
  provide a mirrored file system.
- Support for running x64 (Intel) binary on arm (Silicon) macOS using mirrord. This will download and use the x64
  mirrord-layer binary when needed.
- Add detours for fcntl/dup system calls, closes [#51](https://github.com/metalbear-co/mirrord/issues/51)

### Changed

- Add graceful exit for library extraction logic in case of error.
- Refactor the CI by splitting the building of mirrord-agent in a separate job and caching the agent image for E2E
  tests.
- Update bug report template to apply to the latest version of mirrord.
- Change release profile to strip debuginfo and enable LTO.
- VS Code extension - update dependencies.
- CLI & macOS: Extract to `/tmp/` instead of `$TMPDIR` as the executed process is getting killed for some reason.

### Fixed

- Fix bug that caused configuration changes in the VS Code extension not to work
- Fix typos

## 2.1.0

### Added

- Prompt user to update if their version is outdated in the VS Code extension or CLI.
- Add support for docker runtime, closes [#95](https://github.com/metalbear-co/mirrord/issues/95).
- Add a keep-alive to keep the agent-pod from exiting, closes [#63](https://github.com/metalbear-co/mirrord/issues/63)

## 2.0.4

Complete refactor and re-write of everything.

- The CLI/VSCode extension now use `mirrord-layer` which loads into debugged process using `LD_PRELOAD`
  /`DYLD_INSERT_LIBRARIES`.
  It hooks some of the syscalls in order to proxy incoming traffic into the process as if it was running in the remote
  pod.
- Mono repo
- Fixed unwraps inside
  of [agent-creation](https://github.com/metalbear-co/mirrord/blob/main/mirrord-layer/src/lib.rs#L75),
  closes [#191](https://github.com/metalbear-co/mirrord/issues/191)
