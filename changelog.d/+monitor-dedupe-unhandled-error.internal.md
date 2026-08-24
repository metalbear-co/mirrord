Report each distinct unhandled error in the local UI once per page load, so a crashing
render loop no longer emits the same error thousands of times.
