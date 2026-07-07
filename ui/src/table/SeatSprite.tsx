import { useEffect, useRef } from 'react'
import { drawSprite, SPRITE_HEIGHT, SPRITE_WIDTH, type SpriteDef } from './sprites'

interface Props {
  sprite: SpriteDef
}

/** Draws a sprite's 16x20 pixel grid onto a native-resolution canvas; CSS
 * (`.seat__sprite`, appended in index.css) scales it up with
 * `image-rendering: pixelated` so the art stays crisp. Pixel data is static
 * per sprite, so it only needs a redraw if the sprite identity itself changes
 * (never happens per seat today, but keeps the effect honest). */
export function SeatSprite({ sprite }: Props) {
  const ref = useRef<HTMLCanvasElement>(null)

  useEffect(() => {
    const canvas = ref.current
    if (canvas) drawSprite(canvas, sprite.rows, sprite.palette)
  }, [sprite])

  return <canvas ref={ref} width={SPRITE_WIDTH} height={SPRITE_HEIGHT} className="seat__sprite" aria-hidden="true" />
}
