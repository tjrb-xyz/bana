#!/bin/bash
# Stand-in for actions/runner's config.sh.
echo "config.sh $* (in $PWD)" >>"$FAKE_LOG"
[[ $1 == remove ]] || echo "runner PATH starts ${PATH%%:*}" >>"$FAKE_LOG"
case $1 in
remove) rm -f .runner ;;
*) [[ " $* " == *" --token REG-TOKEN "* || " $* " == *" --token GIVEN "* ]] || exit 1
   printf '{\n  "agentId": 42,\n  "agentName": "x"\n}\n' >.runner ;;
esac
