import { Composition } from 'remotion'

import { DashboardRecording, type DashboardRecordingProps } from './DashboardRecording'

export const RemotionRoot = () => (
  <>
    <Composition
      id="AdminOverview"
      component={DashboardRecording}
      durationInFrames={180}
      fps={30}
      width={1280}
      height={720}
      defaultProps={{ scene: 'overview' }}
    />
    <Composition
      id="AdminConfiguration"
      component={DashboardRecording}
      durationInFrames={180}
      fps={30}
      width={1280}
      height={720}
      defaultProps={{ scene: 'configuration' }}
    />
  </>
)
