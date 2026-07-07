// Pixel-art data + renderer for the four council characters. Pure data + a
// tiny canvas-drawing function — no React here, so it's trivially reusable
// from any component (and testable in isolation, though it's exercised
// indirectly via SeatSprite since it needs a real <canvas>).

export type SpriteRows = readonly string[]
export type SpritePalette = Readonly<Record<string, string>>

export interface SpriteDef {
  readonly rows: SpriteRows
  readonly palette: SpritePalette
}

export type SeatId = 'claude' | 'codex' | 'gemini' | 'grok'

/** Native grid size — every sprite is a 16x20 character grid, one `fillRect`
 * per non-'.' cell. Scale up for display with CSS (`image-rendering:
 * pixelated`) rather than here, so the art stays crisp at any zoom level. */
export const SPRITE_WIDTH = 16
export const SPRITE_HEIGHT = 20

// CLAUDE — therapist. Warm, calm palette; a stethoscope-less bedside manner.
const CLAUDE_SPRITE: SpriteDef = {
  palette: { O: '#2C2C2A', S: '#EBB98F', W: '#F7F4EC', H: '#6B4A38', C: '#D97757', P: '#5F5E5A', B: '#444441' },
  rows: [
    '................',
    '.....OOOOOO.....',
    '....OHHHHHHO....',
    '...OHHHHHHHHO...',
    '...OSSSSSSSSO...',
    '...OSOOSSOOSO...',
    '...OSSSSSSSSO...',
    '...OSSOOOOSSO...',
    '....OSSSSSSO....',
    '...OWWWWWWWWO...',
    '..OWWOCCCCOWWO..',
    '..OWSOCCCCOSWO..',
    '..OWSOCCCCOSWO..',
    '..OWWOCCCCOWWO..',
    '...OWWWWWWWWO...',
    '...OWWWWWWWWO...',
    '....OPPPPPPO....',
    '....OPP..PPO....',
    '....OBB..BBO....',
    '................',
  ],
}

// CODEX — octopus-surgeon. Teal scrubs, extra arms for extra hands on deck.
const CODEX_SPRITE: SpriteDef = {
  palette: { O: '#2C2C2A', W: '#F7F4EC', E: '#10A37F', Y: '#FFFFFF', M: '#9FE1CB' },
  rows: [
    '................',
    '....OOOOOOOO....',
    '...OWWWWWWWWO...',
    '..OWWWWWWWWWWO..',
    '..OEEEEEEEEEEO..',
    '..OEYOYEEYOYEO..',
    '..OEEEEEEEEEEO..',
    '..OEMMMMMMMMEO..',
    '..OEMMMMMMMMEO..',
    '...OEEEEEEEEO...',
    '..OEEOEEEEOEEO..',
    '..OEEOEEEEOEEO..',
    '..OEEO.OEEO.OEEO',
    '..OEEO.OEEO.OEEO',
    '...OEO..OEO..OEO',
    '...OEO..OEO..OEO',
    '....OO...OO...OO',
    '................',
    '................',
    '................',
  ],
}

// GEMINI — twins. Two heads reviewing in sync, indigo-to-blue gradient scrubs.
const GEMINI_SPRITE: SpriteDef = {
  palette: { O: '#2C2C2A', S: '#EBB98F', H: '#26215C', B: '#4285F4', P: '#5F5E5A', D: '#0C447C' },
  rows: [
    '................',
    '.OHHHO....OHHHO.',
    'OHHHHO....OHHHHO',
    'OSSSHO....OHSSSO',
    'OSOSSO....OSSOSO',
    'OSSSSO....OSSSSO',
    '.OSSO......OSSO.',
    'OBBBBO....OBBBBO',
    'SBBBBO....OBBBBS',
    'OBBBBO....OBBBBO',
    'OBBBBO....OBBBBO',
    '.OPPO......OPPO.',
    '.OPPO......OPPO.',
    '.ODDO......ODDO.',
    '................',
    '................',
    '................',
    '................',
    '................',
    '................',
  ],
}

// GROK — raven. Dark plumage, amber accent; perched rather than standing.
const GROK_SPRITE: SpriteDef = {
  palette: { O: '#1A1A1C', K: '#3F3F46', L: '#6E6E78', Y: '#FFFFFF', B: '#EF9F27' },
  rows: [
    '................',
    '......OOOO......',
    '.....OKKKKO.....',
    '....OKKKKKKO....',
    '....OKYOKKKOBBB.',
    '....OKKKKKKOB...',
    '.....OKKKKO.....',
    '....OKKKKKKO....',
    '...OKKLLLLKKO...',
    '...OKKLLLLKKO...',
    '...OKOLLLLOKO...',
    '...OKKLLLLKKO...',
    '....OKKKKKKO....',
    '.....OKKKKO.....',
    '.....OB..BO.....',
    '.....OB..BO.....',
    '....OBB..BBO....',
    '................',
    '................',
    '................',
  ],
}

export const SPRITES: Readonly<Record<SeatId, SpriteDef>> = {
  claude: CLAUDE_SPRITE,
  codex: CODEX_SPRITE,
  gemini: GEMINI_SPRITE,
  grok: GROK_SPRITE,
}

/** Fills a `<canvas>` with a sprite's pixel grid, one `fillRect` per non-'.'
 * cell. Sets the canvas's intrinsic size to the native 16x20 grid — callers
 * scale the element up with CSS. Safe to call repeatedly (e.g. on remount);
 * clears first so it never double-draws. */
export function drawSprite(canvas: HTMLCanvasElement, rows: SpriteRows, palette: SpritePalette): void {
  const ctx = canvas.getContext('2d')
  if (!ctx) return

  if (canvas.width !== SPRITE_WIDTH) canvas.width = SPRITE_WIDTH
  if (canvas.height !== SPRITE_HEIGHT) canvas.height = SPRITE_HEIGHT
  ctx.clearRect(0, 0, SPRITE_WIDTH, SPRITE_HEIGHT)

  rows.forEach((row, y) => {
    for (let x = 0; x < row.length; x++) {
      const cell = row[x]
      if (cell === '.') continue
      const color = palette[cell]
      if (!color) continue
      ctx.fillStyle = color
      ctx.fillRect(x, y, 1, 1)
    }
  })
}
