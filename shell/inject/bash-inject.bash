# Caelum automatic bash integration. Caelum starts bash as `bash --posix` with ENV
# pointing here (bash only reads ENV in POSIX mode); this undoes that, then does what
# a login shell would: /etc/profile and the first of ~/.bash_profile, ~/.bash_login,
# ~/.profile (which usually sources ~/.bashrc), and finally Caelum's own integration.
if [ -n "${CAELUM_BASH_INJECT-}" ]; then
  builtin unset CAELUM_BASH_INJECT
  # Give child shells the ENV the user had, not ours.
  if [ -n "${CAELUM_BASH_ENV-}" ]; then
    ENV=$CAELUM_BASH_ENV
  else
    builtin unset ENV
  fi
  builtin unset CAELUM_BASH_ENV
  builtin set +o posix
  __caelum_inject_dir=${BASH_SOURCE[0]%/*}
  if [ -z "${HISTFILE-}" ] || [ "$HISTFILE" = "$HOME/.sh_history" ]; then
    HISTFILE=$HOME/.bash_history
  fi
  [ -r /etc/profile ] && builtin source /etc/profile
  for __caelum_f in "$HOME/.bash_profile" "$HOME/.bash_login" "$HOME/.profile"; do
    if [ -r "$__caelum_f" ]; then
      builtin source "$__caelum_f"
      break
    fi
  done
  [ -r "$__caelum_inject_dir/../caelum.bash" ] && builtin source "$__caelum_inject_dir/../caelum.bash"
  builtin unset __caelum_f __caelum_inject_dir
fi
