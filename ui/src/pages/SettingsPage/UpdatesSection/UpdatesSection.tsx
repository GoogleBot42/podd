import { useState } from 'react';
import {
  Alert,
  Box,
  Button,
  Chip,
  CircularProgress,
  FormControl,
  InputLabel,
  MenuItem,
  Select,
  Typography,
} from '@mui/material';
import InfoIcon from '@mui/icons-material/Info';

import Section from '../Section.tsx';
import RollbackButton from './RollbackButton.tsx';
import { ComponentKind, postUpdatesCheck, useUpdates } from '@api/updates.ts';
import { UI_VERSION } from '@lib/version.ts';


// The update tiers of REPLACEMENT_PLAN §9, in the order they matter to a user.
const TIER_LABELS: Record<ComponentKind, string> = {
  app: 'App (podd + UI)',
  os: 'OS image',
  mcu_frozen: 'Frozen MCU',
  mcu_sensor: 'Sensor MCU',
  bootloader: 'Bootloader',
};

// Channels podd releases are cut on (docs/RELEASING.md). The device follows
// exactly one, fixed at start-up from PODD_UPDATER_CHANNEL.
const KNOWN_CHANNELS = ['stable', 'beta'];

const formatCheckedAt = (unixSeconds: number | null) => {
  if (unixSeconds === null) return 'never';
  return new Date(unixSeconds * 1000).toLocaleString();
};

export default function UpdatesSection() {
  const { data, isLoading, refetch } = useUpdates();
  const [checking, setChecking] = useState(false);
  const [checkError, setCheckError] = useState<string | null>(null);

  if (isLoading || !data) {
    return (
      <Section title='Updates'>
        <CircularProgress />
      </Section>
    );
  }

  const updater = data.updater;

  const checkNow = () => {
    setChecking(true);
    setCheckError(null);
    postUpdatesCheck()
      .then(() => refetch())
      .catch((e: unknown) => {
        console.error(e);
        setCheckError('Update check failed — see the updater status below.');
        return refetch();
      })
      .finally(() => setChecking(false));
  };

  // The App tier is only known to the agent once a release was installed
  // through it; on a hand-installed device fall back to the running build.
  const versions = updater?.currentVersions ?? [];
  const hasAppVersion = versions.some(entry => entry.kind === 'app');
  const channel = updater?.channel ?? 'stable';
  const channels = KNOWN_CHANNELS.includes(channel) ? KNOWN_CHANNELS : [...KNOWN_CHANNELS, channel];

  return (
    <Section title='Updates'>
      <Box display='flex' flexDirection='column' gap={ 2 }>

        <Box>
          <Typography variant='body2' sx={ { mb: 1 } }>Installed</Typography>
          <Box display='flex' gap={ 1 } flexWrap='wrap'>
            {
              !hasAppVersion && (
                <Chip label={ `${TIER_LABELS.app}: ${data.daemon.version} (${data.daemon.rev})` } size='small'/>
              )
            }
            {
              versions.map(entry => (
                <Chip
                  key={ entry.kind }
                  label={ `${TIER_LABELS[entry.kind]}: ${entry.version}` }
                  size='small'
                />
              ))
            }
            <Chip label={ `UI: ${UI_VERSION}` } size='small' variant='outlined'/>
          </Box>
        </Box>

        {
          !updater && (
            <Alert severity='info'>
              No update agent is running, so podd can't check for or roll back releases.
              Set <code>PODD_UPDATER_*</code> in a systemd drop-in to enable it — see docs/UPDATING.md.
            </Alert>
          )
        }

        {
          updater && (
            <>
              <Box display='flex' gap={ 1 } alignItems='center' flexWrap='wrap'>
                <Typography variant='body2'>Agent</Typography>
                <Chip
                  label={ updater.enabled ? 'enabled' : 'disabled' }
                  size='small'
                  color={ updater.enabled ? 'success' : 'default' }
                />
                <Chip label={ `${updater.mode} mode` } size='small'/>
                <Chip
                  label={ `checked: ${formatCheckedAt(updater.lastCheckUnix)}` }
                  size='small'
                  color={ updater.lastCheckUnix !== null && !updater.lastCheckOk ? 'warning' : 'default' }
                />
              </Box>

              <FormControl size='small' sx={ { maxWidth: 260 } }>
                <InputLabel id='update-channel-label'>Release channel</InputLabel>
                {
                  // Read-only on purpose: the channel is fixed at start-up from
                  // PODD_UPDATER_CHANNEL and pod-updater has no runtime setter,
                  // so an editable control here would silently do nothing.
                }
                <Select
                  labelId='update-channel-label'
                  label='Release channel'
                  value={ channel }
                  disabled
                >
                  {
                    channels.map(name => (
                      <MenuItem key={ name } value={ name }>{ name }</MenuItem>
                    ))
                  }
                </Select>
              </FormControl>

              {
                updater.available.length > 0 && (
                  <Alert severity='info'>
                    <Typography variant='body2'>Available on { channel }:</Typography>
                    {
                      updater.available.map(component => (
                        <Typography key={ `${component.kind}-${component.name}` } variant='body2'>
                          { TIER_LABELS[component.kind] } { component.version }
                        </Typography>
                      ))
                    }
                    <Typography variant='body2' sx={ { mt: 1 } }>
                      { updater.mode === 'auto'
                        ? 'The agent applies app updates on its own.'
                        : 'Apply it from the Pod: re-run the installer (docs/UPDATING.md).' }
                    </Typography>
                  </Alert>
                )
              }

              {
                updater.lastApplied && (
                  <Typography variant='body2' color='text.secondary'>
                    Last applied: { updater.lastApplied }
                  </Typography>
                )
              }

              {
                updater.lastError && (
                  <Alert severity='warning'>{ updater.lastError }</Alert>
                )
              }

              {
                checkError && (
                  <Alert severity='error' onClose={ () => setCheckError(null) }>{ checkError }</Alert>
                )
              }

              <Box display='flex' gap={ 1 } alignItems='center'>
                <Button
                  variant='outlined'
                  size='small'
                  disabled={ checking }
                  onClick={ checkNow }
                >
                  { checking ? 'Checking…' : 'Check now' }
                </Button>
                <RollbackButton onDone={ () => void refetch() }/>
              </Box>
            </>
          )
        }

        <Box display='flex' gap={ 1 }>
          <InfoIcon sx={ { color: 'text.secondary' } }/>
          <Typography variant='body2' color='text.secondary'>
            App updates are atomic and health-checked: a release that doesn't come up healthy is
            rolled back on its own. Roll back manually to return to the previous release — podd
            restarts to do it.
          </Typography>
        </Box>

      </Box>
    </Section>
  );
}
