#!/bin/bash
echo "svc.sh $* (in $PWD)" >>"$FAKE_LOG"
[[ ${FAKE_SVC_FAIL:-} != 1 ]]
