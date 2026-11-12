# Caelum shell integration (OSC 7 cwd + OSC 133 prompt marks).
# Caelum loads this automatically; sourcing it by hand (from ~/.bashrc) is harmless:
#   [ -n "$CAELUM_RS" ] && source /path/to/caelum.bash
[ -n "$CAELUM_RS" ] || return 0
# Already loaded (by hand or by Caelum's automatic injection): do nothing twice.
declare -F __caelum_precmd >/dev/null 2>&1 && return 0
# Percent-encode every byte outside [A-Za-z0-9/._~-] so control bytes (BEL, ESC),
# '%', spaces and non-ASCII bytes in a directory name cannot end or inject sequences.
__caelum_urlencode() {
  local LC_ALL=C s=$1 c h i n
  REPLY=
  for (( i = 0; i < ${#s}; i++ )); do
    c=${s:i:1}
    case $c in
      [A-Za-z0-9/._~-]) REPLY+=$c ;;
      # bash 3.2 (macOS) sign-extends bytes >= 0x80 in "'c", so mask to one byte.
      *) printf -v n '%d' "'$c"; printf -v h '%%%02X' $(( n & 255 )); REPLY+=$h ;;
    esac
  done
}
__caelum_osc7() {
  local REPLY
  __caelum_urlencode "$PWD"
  printf '\e]7;file://%s%s\a' "${HOSTNAME}" "$REPLY"
}
__caelum_precmd() { local s=$?; printf '\e]133;D;%s\a' "$s"; __caelum_osc7; printf '\e]133;A\a'; return $s; }
PROMPT_COMMAND="__caelum_precmd${PROMPT_COMMAND:+;$PROMPT_COMMAND}"
if (( BASH_VERSINFO[0] > 4 || (BASH_VERSINFO[0] == 4 && BASH_VERSINFO[1] >= 4) )); then
  PS0='\e]133;C\a'"${PS0}"
else
  # PS0 only exists from bash 4.4; macOS ships 3.2. Emit the command-start mark from a
  # DEBUG trap instead, armed by the last PROMPT_COMMAND entry so it fires once, just
  # before the first command of the line the user submitted.
  __caelum_armed=
  __caelum_arm() { __caelum_armed=1; }
  __caelum_preexec() { [ -n "$__caelum_armed" ] || return 0; __caelum_armed=; printf '\e]133;C\a'; }
  # Keep a DEBUG trap the user already set (trap -p prints: trap -- 'cmd' DEBUG).
  __caelum_prev=$(trap -p DEBUG)
  if [ -n "$__caelum_prev" ]; then
    __caelum_prev=${__caelum_prev#"trap -- '"}; __caelum_prev=${__caelum_prev%"' DEBUG"}
    trap "__caelum_preexec; $__caelum_prev" DEBUG
  else
    trap '__caelum_preexec' DEBUG
  fi
  unset __caelum_prev
  PROMPT_COMMAND="$PROMPT_COMMAND;__caelum_arm"
fi
