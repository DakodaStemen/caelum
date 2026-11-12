# Caelum automatic zsh integration. Caelum starts zsh with ZDOTDIR pointing at this
# directory; this file puts the user's ZDOTDIR back (so zsh reads their own .zprofile,
# .zshrc and .zlogin), runs their .zshenv, and arranges for Caelum's integration to
# load once the first prompt is about to be drawn, i.e. after their .zshrc.
__caelum_inject_dir=${${(%):-%x}:A:h}
if [[ -n $CAELUM_ZDOTDIR ]]; then
  ZDOTDIR=$CAELUM_ZDOTDIR
else
  unset ZDOTDIR
fi
unset CAELUM_ZDOTDIR
if [[ -r ${ZDOTDIR:-$HOME}/.zshenv && ${ZDOTDIR:-$HOME} != $__caelum_inject_dir ]]; then
  source ${ZDOTDIR:-$HOME}/.zshenv
fi
if [[ -o interactive && -r $__caelum_inject_dir/../caelum.zsh ]]; then
  __caelum_boot() {
    local st=$?
    add-zsh-hook -d precmd __caelum_boot
    unfunction __caelum_boot
    if (( ! $+functions[__caelum_precmd] )); then
      source $__caelum_inject_dir/../caelum.zsh
      # This prompt's precmd missed the new hook: emit its marks now.
      (( $+functions[__caelum_precmd] )) && { (exit $st); __caelum_precmd }
    fi
    unset __caelum_inject_dir
  }
  autoload -Uz add-zsh-hook
  add-zsh-hook precmd __caelum_boot
else
  unset __caelum_inject_dir
fi
