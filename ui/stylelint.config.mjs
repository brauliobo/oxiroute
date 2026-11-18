const USE_A_TOKEN =
  'Use a design token (var(--ox-*) from src/design/tokens) or a primitive from src/design instead of a raw value.'
const USE_A_PRIMITIVE =
  'Fix the cause: build it on a design primitive from src/design or add the missing token, do not patch around it here.'

const tokenValue = (what) => (property, value) => `${property}: ${value} is a raw ${what}. ${USE_A_TOKEN}`

const TOKEN = '/var\\(--ox-/'
const KEYWORDS = '/^(none|0|transparent|currentcolor|inherit|initial|unset|Canvas|CanvasText|Highlight|HighlightText|LinkText)$/i'

const colorProperties =
  '/^(color|background|background-color|border|border-(top|right|bottom|left)|border-color|border-(top|right|bottom|left)-color|outline|outline-color|fill|stroke|caret-color|accent-color|box-shadow|text-decoration-color)$/'
const spacingProperties = '/^(margin|padding)(-(top|right|bottom|left|inline|block)(-(start|end))?)?$|^(gap|row-gap|column-gap)$/'
const fontProperties = '/^(font|font-size|font-family|font-weight|line-height|letter-spacing)$/'
const radiusProperties = '/^border(-(top|bottom)-(left|right))?-radius$/'

export default {
  reportNeedlessDisables: true,
  rules: {
    'color-no-hex': [true, { message: `Hex colours are only allowed in src/design/tokens. ${USE_A_TOKEN}` }],
    'function-disallowed-list': [
      ['rgb', 'rgba', 'hsl', 'hsla', 'hwb', 'lab', 'lch', 'oklab', 'oklch', 'url', 'v-bind'],
      { message: (name) => `${name}() is not allowed here. ${USE_A_TOKEN} Nothing is loaded from the network or bound from script.` },
    ],
    'at-rule-disallowed-list': [['import'], { message: `@import is only allowed in src/design/index.css. ${USE_A_PRIMITIVE}` }],
    'declaration-no-important': [true, { message: `!important is only allowed in src/design/base. ${USE_A_PRIMITIVE}` }],
    'selector-class-pattern': [
      '^ox-[a-z0-9]+(-[a-z0-9]+)*$',
      { message: (name) => `Global class ${name} must start with ox-. ${USE_A_PRIMITIVE}` },
    ],
    'no-duplicate-selectors': [true, { message: (selector) => `${selector} is styled twice in this file. Merge the rules.` }],
    'media-feature-name-value-allowed-list': [
      { 'max-width': ['45rem', '64rem'], 'min-width': ['80rem'] },
      { message: (name, value) => `${name}: ${value} is not a design breakpoint. Use 45rem, 64rem or 80rem.` },
    ],
    'declaration-property-value-allowed-list': [
      {
        [colorProperties]: [TOKEN, KEYWORDS, '/^color-mix\\(/', '/^\\S+ (solid|dotted|dashed) (currentcolor|transparent)$/'],
        [spacingProperties]: [TOKEN, '/^(0|auto)( (0|auto))*$/', '/^inherit$/', '/^-1px$/'],
        [fontProperties]: [TOKEN, '/^(inherit|normal|initial|0)$/'],
        [radiusProperties]: [TOKEN, '/^(0|50%|inherit)$/'],
        'z-index': [TOKEN, '/^(auto|0)$/'],
      },
      { message: tokenValue('colour, spacing, font, radius or z-index value') },
    ],
  },
  overrides: [
    { files: ['**/*.vue'], customSyntax: 'postcss-html' },
    {
      files: ['src/design/tokens/**'],
      rules: {
        'color-no-hex': null,
        'function-disallowed-list': [['url', 'v-bind'], { message: 'Nothing is loaded from the network or bound from script.' }],
        'declaration-property-value-allowed-list': null,
      },
    },
    { files: ['src/design/index.css'], rules: { 'at-rule-disallowed-list': null } },
    { files: ['src/design/base/**'], rules: { 'declaration-no-important': null } },
  ],
}
