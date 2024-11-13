# twrp_evacuate

This is a tool to help you to migrate your TWRP backup into [Neo Backup](https://github.com/NeoApplications/Neo-Backup) (formerly OAndBackupX) format.

When TWRP creates a corrupt backup or fails to restore it for any reason, or you want to restore a backup from a different device, you can use this tool to extract the data from the TWRP backup and convert it into a format that Neo Backup can restore.

Tested on Windows. Linux and macOS should work too.

## Usage

`./twrp_evacuate.exe <path to data.ext4.win000 file>`

Example:

`./twrp_evacuate.exe /path/to/TWRP/BACKUPS/d5591b42/2024-11-13--10-13-38_QQ3A200905001/data.ext4.win000`

Migrated backup will be saved in your current directory (where you run the tool) with the name `twrp_evacuate_migrated`.

Copy `twrp_evacuate_migrated/0` to your device and restore it with Neo Backup.

If you have more than one user (e.g. work profile), you can find the other users' data in the respective directories (e.g. `twrp_evacuate_migrated/10`, `twrp_evacuate_migrated/11`, etc.)

__WARNING: Do not restore all backups at once!__ The migrated backups may contain system apps and data that are not compatible with your device. Restore only the apps you need.

## Building

`cargo build --release`

## Known issues

See [issues](https://github.com/CatMe0w/twrp_evacuate/issues).

## License

MIT License
