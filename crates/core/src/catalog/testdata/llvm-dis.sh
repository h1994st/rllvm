#!/bin/sh
# The test helper selects the exit status through a symlink named llvm-dis-N.
printf '%s\n' 'source_filename = "src/main.c"' 'target triple = "x86_64-test"' 'target datalayout = "e-p:64:64"'
exit "${0##*-}"
