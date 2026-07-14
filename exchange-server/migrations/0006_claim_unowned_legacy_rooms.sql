INSERT INTO marketforge_users (user_id)
VALUES ('local-user')
ON CONFLICT (user_id) DO NOTHING;

INSERT INTO marketforge_room_members (room_id, user_id, role)
SELECT room.room_id, 'local-user', 'owner'
FROM marketforge_rooms AS room
WHERE NOT EXISTS (
    SELECT 1
    FROM marketforge_room_members AS member
    WHERE member.room_id = room.room_id
)
ON CONFLICT (room_id, user_id) DO NOTHING;
