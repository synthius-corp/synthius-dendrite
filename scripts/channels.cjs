const profiles=require('../distribution/profiles.json');
function validateReleaseTag(tag,version,channel='production') {
  if (!['production','preview'].includes(channel)) throw new Error('Unknown release channel');
  if (!/^\d+\.\d+\.\d+$/.test(version)) throw new Error('Invalid base release version');
  const escaped=version.replaceAll('.', '\\.');
  const pattern=channel==='preview'?new RegExp(`^v${escaped}-preview\\.[1-9]\\d*$`):new RegExp(`^v${escaped}$`);
  if (typeof tag!=='string' || !pattern.test(tag)) throw new Error('Release tag does not match the version/channel');
  return tag.slice(1);
}
module.exports={validateReleaseTag,profiles};
