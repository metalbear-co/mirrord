Run the IntelliJ and VSCode end-to-end tests on release pull requests again.
The check that recognises a release branch still looked for a bare version
number, so it stopped matching when those branches were renamed, and both test
suites had been quietly skipping ever since.
