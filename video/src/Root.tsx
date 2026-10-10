import { Composition } from 'remotion';
import { Demo, demoDurationInFrames, fps } from './Demo';

export const Root = () => (
  <Composition
    id="Demo"
    component={Demo}
    durationInFrames={demoDurationInFrames}
    fps={fps}
    width={1920}
    height={1080}
  />
);
