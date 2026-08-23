import { Box, Chip, Typography } from '@mui/material';
import { useDeviceStatus } from '@api/deviceStatus.ts';
import { Version } from '@api/deviceStatusSchema';
import WifiStrength from './WifiStrength.tsx';
import RebootButton from './RebootButton.tsx';


const isUnidentified = (version: string) =>
  version === Version.NotFound || version === Version.Unknown;

export default function DeviceInfo() {
  const { data: deviceStatus, isLoading } = useDeviceStatus();
  if (isLoading || !deviceStatus) return null;
  const hideCover = isUnidentified(deviceStatus.coverVersion);
  const hideHub = isUnidentified(deviceStatus.hubVersion);

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
        <Chip label={ `v${deviceStatus?.freeSleep?.version}` } size='small'/>
        <Chip label={ deviceStatus?.freeSleep?.branch } size='small'/>
      </Box>
      <Box sx={ { display: 'flex', gap: 1, mt: 1 } }>
        <RebootButton />
        <WifiStrength />
      </Box>
    </>
  );
}
