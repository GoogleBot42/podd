import discordIcon from './discord.svg';
import { Box, Link } from '@mui/material';
import Section from './Section.tsx';
export default function DiscordLink() {
  const discordInviteLink = 'https://discord.gg/JpArXnBgEj';

  return (
    <Section title='Support & Feature Requests'>
      <Box sx={ { textAlign: 'center' } }>
        <Link href={ discordInviteLink } target="_blank" rel="noopener noreferrer">
        Join us on Discord!
        </Link>
        <br />
        <Link href={ discordInviteLink } target="_blank" rel="noopener noreferrer">
          <img src={ discordIcon } alt="Join our Discord" width={ 100 } height={ 100 }/>
        </Link>
      </Box>
    </Section>
  );
}
