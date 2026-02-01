.NET 10 switched to using $NOCANCEL variants of the libc functions.
A full list of functions affected can be found under 
[this issue](https://github.com/dotnet/runtime/issues/117299).
For each function in the list that `mirrord` already hooks a detour,
`mirrord` now hooks the $NOCANCEL variant as well.
