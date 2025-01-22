# Jackson Coxson + ny

import asyncio
import logging
import aiosqlite

from pymobiledevice3.services.mobile_image_mounter import auto_mount_personalized
from pymobiledevice3.tunneld.api import async_get_tunneld_device_by_udid

logging.basicConfig(level=logging.INFO)


async def process_mount_queue():
    """
    Reads from the SQLite database and processes pending mount jobs.
    """
    db_path = "jitstreamer.db"

    async with aiosqlite.connect(db_path) as db:
        while True:
            # Fetch the next pending job
            async with db.execute(
                """
                SELECT udid, ordinal
                FROM mount_queue
                WHERE status = 0
                ORDER BY ordinal ASC
                LIMIT 1
                """
            ) as cursor:
                row = await cursor.fetchone()

            if not row:
                logging.info("No pending mounts. Retrying in 1 seconds...")
                await asyncio.sleep(1)
                continue

            udid, ordinal = row

            # Claim the job by setting status to 1
            await db.execute(
                "UPDATE mount_queue SET status = 1 WHERE ordinal = ?",
                (ordinal,),
            )
            await db.commit()

            logging.info(f"Claimed mount job for UDID: {udid}, Ordinal: {ordinal}")

            try:
                # Get the device
                device = await async_get_tunneld_device_by_udid(udid)
                if not device:
                    raise Exception(f"Device {udid} not found")

                # Process the mount
                result = await auto_mount_personalized(device)
                logging.info(result)

                # Delete the device from the queue
                await db.execute(
                    "DELETE FROM mount_queue WHERE ordinal = ?",
                    (ordinal,),
                )
            except Exception as e:
                logging.error(f"Error mounting device {udid}: {e}")

                # Update the database with the error
                await db.execute(
                    "UPDATE mount_queue SET status = 2, error = ? WHERE ordinal = ?",
                    (str(e), ordinal),
                )

            await db.commit()
            logging.info(f"Finished processing ordinal {ordinal}")


if __name__ == "__main__":
    try:
        asyncio.run(process_mount_queue())
    except KeyboardInterrupt:
        logging.info("Shutting down gracefully...")
