import hashlib
import json
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch
from marketforge.agents.program_sdk import Client
from python.tests import test_agent_runtime as fixture

class WorkspaceRuntimeTests(unittest.TestCase):
    setUp=fixture.RuntimeTests.setUp
    tearDown=fixture.RuntimeTests.tearDown
    def request(self,ident='buy',**args):
        return {'id':ident,'name':'trade','arguments':{'instrument':'PERP','action':'fok','side':'Buy','price_tick':100,'qty':1,**args}}
    def running(self):
        config=self.runtime.config('alice');config['status']='running';self.runtime.store.put('trader','alice',config)
    def test_program_pause_scope_stable_intent_and_shared_budget(self):
        with self.assertRaisesRegex(ValueError,'paused'):self.runtime.program_call('alice',self.request())
        self.running()
        first=self.runtime.program_call('alice',self.request())
        self.assertEqual(first,self.runtime.program_call('alice',self.request()))
        self.assertEqual(len(self.exchange.orders),1)
        self.assertEqual(self.exchange.orders[0]['account_id'],20)
        self.assertIn('error',self.runtime.program_call('alice',self.request('wrong',instrument='OTHER')))
        with self.assertRaises(ValueError):self.runtime.program_call('alice',self.request('spoof',account_id=30))
        config=self.runtime.config('alice');config['orders_per_minute']=1;self.runtime.store.put('trader','alice',config)
        self.assertIn('error',self.runtime.program_call('alice',self.request('new-order')))
        self.assertEqual(len(self.exchange.orders),1)
    def test_program_offline_and_urgent_alert_block_admission(self):
        self.running()
        with patch.object(self.runtime,'connection_live',return_value=False):
            with self.assertRaisesRegex(ValueError,'offline'):self.runtime.program_call('alice',self.request())
        with patch.object(self.runtime.alerts,'state',return_value={'pending':[{'name':'risk'}]}):
            with self.assertRaisesRegex(ValueError,'urgent alert'):self.runtime.program_call('alice',self.request())
        self.assertFalse(self.exchange.orders)
    def test_workspace_cannot_use_operator_or_framework_tools(self):
        for name in ('decision_begin','workspace_exec','strategy_save','alert_set'):
            with self.assertRaisesRegex(ValueError,'unavailable'):self.runtime.program_call('alice',{'id':'a','name':name,'arguments':{}})
    def test_decimal_native_order_id_and_reducing_order_variants(self):
        config=self.runtime.config('alice');large='8000000000000000001'
        self.assertEqual(self.runtime.validate_trade(config,{'instrument':'PERP','action':'cancel','order_id':large},'workspace'),{'Cancel':{'order_id':int(large)}})
        for action,kind in (('reduce_only_limit','Limit'),('reduce_only_post_only','PostOnly')):
            value=self.runtime.validate_trade(config,{'instrument':'PERP','action':action,'side':'Sell','price_tick':110,'qty':1},'workspace')['PlaceProtected']
            self.assertTrue(value['reduce_only']);self.assertEqual(value['order_type'],kind)
        with self.assertRaises(ValueError):self.runtime.validate_trade(config,{'instrument':'PERP','action':'cancel','order_id':str(2**64)},'workspace')
    def test_policy_enable_rejects_armed_native_conditions(self):
        with patch.object(self.runtime,'advanced_execute',return_value={'conditionals':[{'status':'armed'}]}):
            with self.assertRaisesRegex(ValueError,'cancel armed'):self.runtime.policy_update('alice',{'account':{'PERP':{'max_abs_position':5}},'model':{}})
        self.assertEqual(self.runtime.policy('alice')['account'],{})
    def test_native_condition_cannot_bypass_current_account_policy(self):
        self.runtime.store.put('policy','alice',{'account':{'PERP':{'max_abs_position':5}},'model':{}})
        args={'instrument':'PERP','action':'conditional','conditional_key':'entry','conditional_spec':{'side':'Buy','above':True,'qty':1,'trigger_price_tick':110}}
        with self.assertRaisesRegex(ValueError,'disabled'):self.runtime.validate_trade(self.runtime.config('alice'),args,'workspace')
        args['conditional_spec']=None
        self.runtime.check_account_policy(self.runtime.config('alice'),args)

class ProgramSDKTests(unittest.TestCase):
    def test_reply_replay_checks_exact_payload_and_no_duplicate_queue(self):
        with tempfile.TemporaryDirectory() as directory:
            client=Client(.01);client.root=Path(directory);(client.root/'responses').mkdir()
            args={'instrument':'PERP','action':'fok'}
            digest=hashlib.sha256(json.dumps({'name':'trade','arguments':args},sort_keys=True,separators=(',',':')).encode()).hexdigest()
            (client.root/'responses'/'known.json').write_text(json.dumps({'input_hash':digest,'result':{'accepted':True}}))
            self.assertTrue(client.call('trade',args,'known')['accepted'])
            self.assertFalse((client.root/'requests'/'known.json').exists())
            with self.assertRaisesRegex(ValueError,'different'):client.call('trade',{**args,'qty':2},'known')
    def test_unknown_result_keeps_request_intent_for_retry(self):
        with tempfile.TemporaryDirectory() as directory:
            client=Client(.001);client.root=Path(directory)
            with self.assertRaisesRegex(TimeoutError,'unknown'):client.call('trade',{'qty':1},'stable')
            queued=json.loads((client.root/'requests'/'stable.json').read_text());self.assertEqual(queued['id'],'stable')
            with self.assertRaisesRegex(ValueError,'different'):client.call('trade',{'qty':2},'stable')
            self.assertEqual(json.loads((client.root/'requests'/'stable.json').read_text()),queued)
    def test_watch_retries_unknown_reads_with_the_same_request_id(self):
        client=Client();requests=[]
        def call(name,args,request_id=None):
            requests.append((name,args.copy(),request_id))
            if len(requests)==1:raise TimeoutError('unknown')
            return {'activities':[],'next_after_command_seq':7,'has_more':False}
        client.call=call
        with patch('marketforge.agents.program_sdk.time.sleep'):
            stream=client.watch('PERP',after_command_seq=5)
            self.assertEqual(next(stream)['next_after_command_seq'],7)
            stream.close()
        self.assertEqual(requests[0],requests[1])
    def test_watch_does_not_swallow_a_known_scope_error(self):
        client=Client();client.call=lambda *args:{'error':'instrument not assigned'}
        with self.assertRaisesRegex(RuntimeError,'not assigned'):next(client.watch('OTHER'))
    def test_full_history_uses_cursor_even_when_scanned_page_has_no_own_events(self):
        client=Client();calls=[]
        pages=[{'activities':[],'next_after_command_seq':5,'has_more':True},{'activities':[{'command_seq':8}],'next_after_command_seq':10,'has_more':False}]
        def call(name,args,request_id=None):calls.append(args.copy());return pages.pop(0)
        client.call=call
        result=list(client.iter_history('PERP',order_id='1'))
        self.assertEqual(len(result),2);self.assertTrue(calls[0]['from_start']);self.assertNotIn('from_start',calls[1]);self.assertEqual(calls[1]['after_command_seq'],5)
if __name__=='__main__':unittest.main()
