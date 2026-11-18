#!/usr/bin/env bash
# Sourced by the scripts/verify/ui checks: the console stylesheets they police and the text of their rules. The design
# system, the stats page and the gallery are not listed: stylelint holds them to the rules without a baseline.

console_styles() {
  git -C "$1" ls-files -z -- 'ui/src/*.css' 'ui/src/*.vue' | grep -zvE '^ui/src/(design|stats|gallery)/'
}

style_text() {
  case $2 in
    *.css) cat -- "$1/$2" ;;
    *.vue) awk '/<style/ { inside = 1; next } /<\/style>/ { inside = 0 } inside' "$1/$2" ;;
  esac
}

declaration_text() {
  style_text "$1" "$2" | sed -E 's/^[^{]*\{//'
}

selector_text() {
  style_text "$1" "$2" | grep -E '\{|,\s*$' | grep -vE '^\s*@' | sed -E 's/\{.*$//'
}
