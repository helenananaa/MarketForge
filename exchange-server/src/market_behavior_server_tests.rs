#[test]
fn expired_live_bot_decision_is_journaled_and_recovered_without_stopping_market() {
    use exchange_core::{AgentTemplate, BotConfig, ParticipantConfig, ParticipantKind, SchedulerMode, SchedulerState};
    let scenario = spot_scenario("expired-live-bot");
    let mut rooms = RoomManager::new();
    let bootstrap = rooms.create_room(scenario.clone()).unwrap();
    let mut store = journal::InMemoryJournalStore::new();
    store.create_room("owner", &scenario, &bootstrap, &[10,20], &[], None).unwrap();
    let template = AgentTemplate::Plugin(BotConfig {
        participant: ParticipantConfig { participant_id:"expired-pov".into(),kind:ParticipantKind::RuleAgent,room_id:scenario.room_id.clone(),account_id:20,instrument_id:Some("V-BTC-SPOT".into()) },
        plugin_id:"PovExecutionTrader".into(),plugin_version:"1".into(),state_version:1,config_version:1,seed:7,config:serde_json::json!({}),
    });
    let scheduler = SchedulerState::new(&scenario.room_id, vec![template], SchedulerMode::Auto{interval_ms:25});
    let before_clock = rooms.simulation_room(&scenario.room_id).unwrap().next_command_seq();
    rooms.advance_clock(&scenario.room_id,2).unwrap();
    store.append_room_mutation(&PendingJournalMutation::new(&scenario.room_id,before_clock,RoomMutation::ClockAdvanced{steps:2,completed_transfers:vec![]}),&[],&[],None).unwrap();
    let prior = scheduler.agents[0].clone();
    let clock = rooms.clock(&scenario.room_id).unwrap();
    let action = OrderAction::PlaceProtected {
            position_side: Default::default(),side:Side::Buy,qty:1,price_tick:100,order_type:exchange_core::model::ProtectedOrderType::ImmediateOrCancel,reduce_only:false,valid_until_market_time_ms:Some(1000),expires_at_market_time_ms:None};
    let work = realtime::Work::Bot {control:Default::default(),epoch:0,decision:Box::new(realtime::Decision{next:prior.kind_state.clone(),prior,actions:vec![action]})};
    let before = rooms.simulation_room(&scenario.room_id).unwrap().next_command_seq();
    let mut order_id = 1000;
    let mut submissions = BTreeMap::new();
    let mut outcome = work.apply(&mut rooms,&mut order_id,&scheduler,&mut ServerBotPolicy{training:None},&mut submissions).unwrap();
    outcome.state.revision = scheduler.revision + 1;
    let mut recovered_state = scheduler;
    outcome.apply_owned(&mut recovered_state).unwrap();
    let execution = rooms.execution_history(&scenario.room_id).unwrap().last().unwrap().clone();
    assert!(matches!(execution.result,ActorExecutionResult::Rejected(exchange_core::ActorRejectReason::OrderProtectionExpired{..})));
    let command = command_from_actor_execution(&execution).expect("actor rejection retains submitted command");
    let (participant,account) = submissions.get(&execution.command_seq).unwrap().clone();
    let record = JournalExecution::submitted(participant,account,command,execution);
    store.append_room_mutation(&PendingJournalMutation::new(&scenario.room_id,before,RoomMutation::SchedulerProgress{clock_steps:0,state:recovered_state,training:None}),&[record],&[],None).unwrap();
    let recovered = recover_rooms(&store.load_recovery().unwrap()).unwrap();
    assert_eq!(recovered.book_snapshot(&scenario.room_id).unwrap(),rooms.book_snapshot(&scenario.room_id).unwrap());
    assert_eq!(recovered.clock(&scenario.room_id).unwrap(),clock);
    assert_eq!(recovered.status(&scenario.room_id).unwrap(),MarketStatus::Running);
    assert_eq!(order_id,1001);
}
