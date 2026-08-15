import { Box, Link, Typography } from '@mui/material';
import Section from './Section.tsx';

export default function AboutSection() {
  return (
    <Section title='About'>
      <Box sx={ { display: 'flex', flexDirection: 'column', gap: 1 } }>
        <Typography variant='body2' color='text.secondary'>
          podd is free and open-source replacement firmware for the Eight Sleep
          Pod. It runs entirely on your local network — no cloud, no
          subscription, no telemetry.
        </Typography>
        <Typography variant='body2' color='text.secondary'>
          Source code and issue tracker:&nbsp;
          <Link href='https://git.neet.dev/zuckerberg/podd' target='_blank' rel='noopener noreferrer'>
            git.neet.dev/zuckerberg/podd
          </Link>
        </Typography>
        <Typography variant='body2' color='text.secondary'>
          This web app is based in part on&nbsp;
          <Link href='https://github.com/throwaway31265/free-sleep' target='_blank' rel='noopener noreferrer'>
            free-sleep
          </Link>
          &nbsp;by throwaway31265 and contributors (MIT licensed) — thank you!
        </Typography>
      </Box>
    </Section>
  );
}
