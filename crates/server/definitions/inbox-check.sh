# Whether the lab inbox on port $1 holds a message matching $2, in
# Mailpit's own search words (to:ana@lab subject:"Reset" ...; empty for
# any message). Exit 0 when one does, 1 when none does, 2 when the inbox
# cannot be asked. Bash alone: the guest is only promised bash and mailpit.
set -u
export LC_ALL=C
port=$1
search=$2
encode() {
  local text=$1 out= char i
  for ((i = 0; i < ${#text}; i++)); do
    char=${text:i:1}
    case $char in
      [a-zA-Z0-9.~_-]) out+=$char ;;
      *) printf -v char '%%%02X' "'$char"; out+=$char ;;
    esac
  done
  printf '%s' "$out"
}
if [ -n "$search" ]; then
  path="/api/v1/search?query=$(encode "$search")&limit=1"
else
  path="/api/v1/messages?limit=1"
fi
if ! { exec 3<>"/dev/tcp/127.0.0.1/$port"; } 2>/dev/null; then
  echo "The inbox does not answer on port $port." >&2
  exit 2
fi
printf 'GET %s HTTP/1.0\r\nHost: localhost\r\nConnection: close\r\n\r\n' "$path" >&3
reply=$(cat <&3)
body=${reply#*$'\r\n\r\n'}
count='"messages_count":([0-9]+)'
if [[ ${reply%%$'\r'*} != HTTP/*' 200'* || ! $body =~ $count ]]; then
  echo "The inbox gave an unexpected answer: ${reply%%$'\r'*}" >&2
  exit 2
fi
count=${BASH_REMATCH[1]}
if [ "$count" -eq 0 ]; then
  echo "No message matches."
  exit 1
fi
subject='"Subject":"(([^"\\]|\\.)*)"'
to='"To":\[\{"Name":"([^"\\]|\\.)*","Address":"(([^"\\]|\\.)*)"'
[[ $body =~ $subject ]] && subject=${BASH_REMATCH[1]} || subject=
[[ $body =~ $to ]] && to=${BASH_REMATCH[2]} || to=
echo "$count matching message(s). Newest: \"${subject//\\\"/\"}\"${to:+ to $to}"
