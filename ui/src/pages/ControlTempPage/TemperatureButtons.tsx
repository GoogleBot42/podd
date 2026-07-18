import { useRef, useCallback } from 'react';
import { useTheme } from '@mui/material/styles';
import { Button, Box } from '@mui/material';
import { Add, Remove } from '@mui/icons-material';
import { useControlTempStore } from './controlTempStore.tsx';
import { useAppStore } from '@state/appStore.tsx';
import { postDeviceStatus } from '@api/deviceStatus.ts';
import { useSettings } from '@api/settings.ts';
import { MIN_TEMP_F, MAX_TEMP_F } from '@lib/temperatureConversions.ts';

type TemperatureButtonsProps = {
  refetch: any;
  currentTargetTemp: number;
}

const DEBOUNCE_MS = 2000;
export default function TemperatureButtons({ refetch, currentTargetTemp }: TemperatureButtonsProps) {
  const { side, setIsUpdating, isUpdating } = useAppStore();
  const { deviceStatus, setDeviceStatus } = useControlTempStore();
  const { data: settings } = useSettings();
  const theme = useTheme();
  const debounceTimer = useRef<ReturnType<typeof setTimeout> | null>(null);

  const postUpdate = useCallback(async () => {
    setIsUpdating(true);
    try {
      await postDeviceStatus({
        [side]: { targetTemperatureF: deviceStatus?.[side]?.targetTemperatureF },
      });
      await new Promise(r => setTimeout(r, 1_500));
      await refetch?.();
    } catch (err) {
      console.error(err);
    } finally {
      setIsUpdating(false);
    }
  }, [deviceStatus, side, refetch, setIsUpdating]);

  const scheduleUpdate = useCallback(() => {
    if (debounceTimer.current) clearTimeout(debounceTimer.current);
    debounceTimer.current = setTimeout(postUpdate, DEBOUNCE_MS);
  }, [postUpdate]);


  const isInAwayMode = settings?.[side].awayMode;
  if (isInAwayMode) return null;

  const disabled = isUpdating || isInAwayMode;
  const borderColor = theme.palette.grey[800];
  const iconColor = theme.palette.grey[500];

  const handleClick = (change: number) => {
    if (!deviceStatus) return;
    if (deviceStatus === undefined) return;
    setDeviceStatus({
      [side]: {
        targetTemperatureF: deviceStatus[side].targetTemperatureF + change,
      }
    });

    scheduleUpdate();
  };

  const buttonStyle = {
    borderWidth: '2px',
    borderColor,
    width: 50,
    height: 50,
    borderRadius: '50%',
    minWidth: 0,
    padding: 0,
  };

  return (
    <Box
      sx={ {
        top: '75%',
        position: 'absolute',
        display: 'flex',
        justifyContent: 'center',
        alignItems: 'center',
        gap: '100px',
        width: '100%',
        marginLeft: 'auto',
        marginRight: 'auto',
      } }
    >
      <Button
        variant="outlined"
        color="primary"
        sx={ buttonStyle }
        onClick={ () => handleClick(-1) }
        disabled={ disabled || currentTargetTemp <= MIN_TEMP_F }
      >
        <Remove sx={ { color: iconColor } }/>
      </Button>
      <Button
        variant="outlined"
        sx={ buttonStyle }

        onClick={ () => handleClick(1) }
        disabled={ disabled || currentTargetTemp >= MAX_TEMP_F }
      >
        <Add sx={ { color: iconColor } }/>
      </Button>
    </Box>
  );
}
