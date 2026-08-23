import { Box, Chip, Typography } from '@mui/material';
import { useDeviceStatus } from '@api/deviceStatus.ts';
import { Version } from '@api/deviceStatusSchema';
import { UI_VERSION } from '@lib/version.ts';
import WifiStrength from './WifiStrength.tsx';
import RebootButton from './RebootButton.tsx';


const isUnidentified = (version: string) =>
  version === Version.NotFound || version === Version.Unknown;

export default function DeviceInfo() {
  const { data: deviceStatus, isLoading } = useDeviceStatus();
  if (isLoading || !deviceStatus) return null;
  const hideCover = isUnidentified(deviceStatus.coverVersion);
  const hideHub = isUnidentified(deviceStatus.hubVersion);
  // `freeSleep.version`/`.branch` are the daemon's build stamp: git describe +
  // short commit (see crates/api/src/wire.rs). UI_VERSION is this bundle's own
  // stamp; the two are built together, so a difference means the binary and the
  // bundle were deployed out of step — worth surfacing, quiet when they agree.
  const daemonVersion = deviceStatus?.freeSleep?.version;
  const daemonRev = deviceStatus?.freeSleep?.branch;
  const uiMismatch = UI_VERSION !== daemonVersion;

  return (
    <>
      <Box sx={ { display: 'flex', gap: 1, mb: 1 } }>
        <Typography variant='body2'>Device</Typography>
        {
          !hideCover && <Chip label={ `${deviceStatus.coverVersion} Cover` } size='small'/>
        }
        {
          !hideHub && <Chip label={ `${deviceStatus.hubVersion} Hub` } size='small'/>
        }
      </Box>
      <Box sx={ { display: 'flex', gap: 1, align: 'center', alignItems: 'center', mb: 1 } }>
        <Typography variant='body2'>podd Build</Typography>
        <Chip label={ `v${daemonVersion}` } size='small'/>
        <Chip label={ daemonRev } size='small'/>
        {
          uiMismatch && <Chip label={ `UI v${UI_VERSION}` } size='small' color='warning'/>
        }
      </Box>
      <Box sx={ { display: 'flex', gap: 1, mt: 1 } }>
        <RebootButton />
        <WifiStrength />
      </Box>
    </>
  );
}
